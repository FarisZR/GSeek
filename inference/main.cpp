// SPDX-License-Identifier: MIT
// GSeek's isolated EmbeddingGemma 2 worker. One normalized embedding per request.
// The parent owns process deadlines and can kill a stuck driver without taking down search or GTK.
#include "llama.h"
#include "ggml-backend.h"
#include "mtmd.h"
#include "mtmd-helper.h"
#include "nlohmann/json.hpp"
#include <algorithm>
#include <cmath>
#include <cstdlib>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

using json = nlohmann::json;
static void require(bool ok, const char *message) { if (!ok) throw std::runtime_error(message); }
static void log_message(ggml_log_level level, const char *text, void *) {
    if (level >= GGML_LOG_LEVEL_WARN) std::cerr << text;
}

class Encoder {
    std::unique_ptr<llama_model, decltype(&llama_model_free)> model{nullptr, llama_model_free};
    std::unique_ptr<llama_context, decltype(&llama_free)> context{nullptr, llama_free};
    std::unique_ptr<mtmd_context, decltype(&mtmd_free)> media{nullptr, mtmd_free};
    std::unique_ptr<llama_batch_ext, decltype(&llama_batch_ext_free)> batch{nullptr, llama_batch_ext_free};
    int position = 0;
    int max_tokens;
    void placed(int index) {
        require(index >= 0, "Input exceeds the embedding context");
        llama_pos pos[] = {position++, 0, 0, 0};
        require(llama_batch_ext_set_pos(batch.get(), index, pos), "Cannot set token position");
        require(llama_batch_ext_set_output_embd(batch.get(), index, true), "Cannot set embedding output");
    }
    void token(llama_token id) { placed(llama_batch_ext_add_token(batch.get(), 0, id)); }
public:
    Encoder(const char *text, const char *projection, const char *backends, bool gpu, int threads) : max_tokens(projection ? 2048 : 512) {
        llama_log_set(log_message, nullptr);
        mtmd_helper_log_set(log_message, nullptr);
        if (backends) ggml_backend_load_all_from_path(backends); else ggml_backend_load_all();
        llama_backend_init();
        if (gpu) {
            bool found = false;
            for (size_t i = 0; i < ggml_backend_dev_count(); ++i) {
                const auto type = ggml_backend_dev_type(ggml_backend_dev_get(i));
                found |= type == GGML_BACKEND_DEVICE_TYPE_GPU || type == GGML_BACKEND_DEVICE_TYPE_IGPU;
            }
            require(found, "No compatible GPU was found for this backend");
        }
        auto mp = llama_model_default_params(); mp.n_gpu_layers = gpu ? 999 : 0;
        model.reset(llama_model_load_from_file(text, mp)); require(bool(model), "Cannot load EmbeddingGemma 2 weights");
        auto cp = llama_context_default_params();
        cp.n_ctx = cp.n_batch = cp.n_ubatch = max_tokens;
        cp.n_seq_max = 1; cp.kv_unified = true; cp.embeddings = true;
        cp.pooling_type = LLAMA_POOLING_TYPE_MEAN;
        cp.n_threads = cp.n_threads_batch = threads;
        context.reset(llama_init_from_model(model.get(), cp)); require(bool(context), "Cannot initialize embedding context");
        if (projection) {
            auto mm = mtmd_context_params_default(); mm.use_gpu = gpu; mm.print_timings = false;
            mm.warmup = false; mm.n_threads = threads; mm.image_min_tokens = 1; mm.image_max_tokens = 1120;
            media.reset(mtmd_init_from_file(projection, model.get(), mm)); require(bool(media), "Cannot load image/audio encoders");
            require(mtmd_support_vision(media.get()) && mtmd_support_audio(media.get()), "Model does not support both images and audio");
        }
        batch.reset(llama_batch_ext_init(context.get())); require(bool(batch), "Cannot allocate embedding batch");
        require(llama_model_n_embd_out(model.get()) == 768, "Wrong embedding dimensions");
    }

    std::vector<float> embed(const json &input) {
        llama_batch_ext_clear(batch.get()); position = 0;
        const auto kind = input.at("kind").get<std::string>();
        if (kind == "text") {
            const auto text = input.at("text").get<std::string>();
            require(text.size() <= 65536, "Text request is too large");
            const auto *vocab = llama_model_get_vocab(model.get());
            std::vector<llama_token> tokens(text.size() + 16);
            int count = llama_tokenize(vocab, text.c_str(), text.size(), tokens.data(), tokens.size(), true, true);
            if (count < 0) { tokens.resize(-count); count = llama_tokenize(vocab, text.c_str(), text.size(), tokens.data(), tokens.size(), true, true); }
            require(count > 0, "Cannot tokenize input");
            tokens.resize(count);
            if (count > max_tokens) { tokens.resize(max_tokens); tokens.back() = llama_vocab_eos(vocab); }
            for (auto id : tokens) token(id);
        } else {
            require(bool(media), "This is a text-only query encoder");
            require(kind == "image" || kind == "audio", "Unsupported input modality");
            const auto path = input.at("path").get<std::string>();
            const auto wrapper = mtmd_helper_bitmap_init_from_file(media.get(), path.c_str(), false, mtmd_helper_init_opt_default());
            std::unique_ptr<mtmd_bitmap, decltype(&mtmd_bitmap_free)> bitmap(wrapper.bitmap, mtmd_bitmap_free);
            if (wrapper.video_ctx) { mtmd_helper_video_free(wrapper.video_ctx); throw std::runtime_error("Video must arrive as frames and sound"); }
            require(bool(bitmap), "Cannot decode media input");
            require(mtmd_bitmap_is_audio(bitmap.get()) == (kind == "audio"), "Media type differs from request");
            const auto *marker = mtmd_default_marker();
            mtmd_input_text text{marker, std::char_traits<char>::length(marker), true, true};
            std::unique_ptr<mtmd_input_chunks, decltype(&mtmd_input_chunks_free)> chunks(mtmd_input_chunks_init(), mtmd_input_chunks_free);
            const mtmd_bitmap *bitmaps[] = {bitmap.get()};
            require(mtmd_tokenize(media.get(), chunks.get(), &text, bitmaps, 1) == 0, "Cannot tokenize media input");
            for (size_t i = 0; i < mtmd_input_chunks_size(chunks.get()); ++i) {
                const auto *chunk = mtmd_input_chunks_get(chunks.get(), i);
                if (mtmd_input_chunk_get_type(chunk) == MTMD_INPUT_CHUNK_TYPE_TEXT) {
                    size_t count = 0; const auto *ids = mtmd_input_chunk_get_tokens_text(chunk, &count);
                    for (size_t t = 0; t < count; ++t) token(ids[t]);
                } else {
                    require(mtmd_encode_chunk(media.get(), chunk) == 0, "Media encoder failed");
                    const auto *rows = mtmd_get_output_embd(media.get()); require(rows != nullptr, "No encoder rows");
                    const auto width = llama_model_n_embd_inp(model.get());
                    const auto count = mtmd_input_chunk_get_n_tokens(chunk);
                    for (size_t t = 0; t < count; ++t) {
                        llama_embd embd{rows + t * width, 1, static_cast<size_t>(width)};
                        placed(llama_batch_ext_add_embd(batch.get(), 0, embd));
                    }
                }
            }
        }
        require(llama_process(context.get(), LLAMA_PROCESS_TYPE_ENCODE, batch.get()) == 0, "Embedding inference failed");
        const auto *pooled = llama_get_embeddings_seq(context.get(), 0); require(pooled != nullptr, "No pooled embedding");
        std::vector<float> result(pooled, pooled + 768); double norm = 0;
        for (float value : result) { require(std::isfinite(value), "Non-finite embedding"); norm += double(value) * value; }
        require(norm > 0, "Zero embedding"); norm = std::sqrt(norm);
        for (float &value : result) value /= norm;
        return result;
    }
};

int main(int argc, char **argv) {
    try {
        require(argc == 6, "Usage: gseek-inference TEXT_MODEL MEDIA_MODEL_OR_- BACKENDS_DIR cpu|gpu THREADS");
        Encoder encoder(argv[1], std::string(argv[2]) == "-" ? nullptr : argv[2], argv[3], std::string(argv[4]) == "gpu", std::clamp(std::atoi(argv[5]), 1, 8));
        std::cout << json({{"ready",true},{"model","google/embeddinggemma-2"},{"dimensions",768},{"protocol",1}}).dump() << std::endl;
        std::string line;
        while (std::getline(std::cin, line)) {
            try {
                require(line.size() <= 131072, "Request is too large");
                const auto request = json::parse(line);
                const auto vector = encoder.embed(request.at("input"));
                std::cout << json({{"id",request.at("id")},{"vector",vector}}).dump() << std::endl;
            } catch (const std::exception &e) { std::cout << json({{"error",e.what()}}).dump() << std::endl; }
        }
        return 0;
    } catch (const std::exception &e) { std::cerr << "GSeek inference: " << e.what() << '\n'; return 1; }
}
