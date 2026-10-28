# GSeek presentation media

[Download the demo video](media/gseek-demo.mp4?raw=1) ·
[Poster](media/demo-poster.jpg) · [Social preview](media/social-preview.png)

The video is **51.5 seconds**, **1920 × 1080 at 30 fps**, H.264 High/yuv420p
with a silent stereo AAC track and MP4 fast-start metadata. It is about **4.3 MB**
and has captions for muted playback. Upload the MP4 itself when posting the demo.
The README uses a separate 9-second, roughly 334 KiB animated preview.

## What the recording shows

| Time | Search | Result opened |
| --- | --- | --- |
| 00:03 | “the little bird dressed for a black tie dinner” | `IMG_0841.jpg`, a penguin photo in GNOME Image Viewer |
| 00:13 | “the video where Earth is wearing a green halo” | `VID_0312.mp4`, aurora footage in GNOME Video Player |
| 00:26 | “can my kitten live with me in the new apartment?” | `Agreement-17.docx`, a tenancy clause in LibreOffice Writer |
| 00:38 | “البطريق الذي يبدو وكأنه يرتدي بدلة رسمية” | The same penguin photo, found with an Arabic query |
| 00:48 | Installation link | The signed Flatpak repository |

This is the signed **GSeek 0.2.3 Flatpak** running in a private **GNOME 51**
Wayland session with the real, verified EmbeddingGemma 2 model and CPU runtime.
The 14-file demonstration library was indexed beforehand and the query model
was warm. This is a product demonstration, not a performance benchmark or a
claim about retrieval accuracy across arbitrary libraries.

Queries are entered automatically in the real Activities search field. Results
come from GSeek's actual SearchProvider2 service, and activating the default
result opens the real file. A GNOME TinySPARQL endpoint supplies only the sample
file metadata; embeddings, rankings, result lists, and opened content are not
mocked. The library includes six fictional DOCX documents, five text notes, two
CC0 photographs, and one NASA video. Filenames do not describe the target media.

The continuous desktop recording runs at normal speed. Editing adds title and
closing cards and query captions; it does not replace results or speed up
searches. The small README animation is a montage of six still frames from that
video. All README screenshots were captured from the same installed app and
sample library. The recording-only Shell extension and desktop automation are
confined to the disposable session and are not part of GSeek.

## Post draft

Attach `gseek-demo.mp4` to this 267-character draft:

```text
I’m building GSeek: local AI file search in GNOME Activities.

“the video where Earth is wearing a green halo”
“can my kitten live with me in the new apartment?”

Photos, videos, documents. GTK4 + libadwaita. No account or telemetry.

https://github.com/FarisZR/GSeek
```

## Media credits

- **Penguin photograph:** [ChrisEdwardsCE, WordPress Photo Directory](https://wordpress.org/photos/photo/8616340578/), [CC0](https://creativecommons.org/publicdomain/zero/1.0/). Used as `IMG_0841.jpg`.
- **Mountain-lake photograph:** [annezazu, WordPress Photo Directory](https://wordpress.org/photos/photo/12261b91fb/), CC0. Used as a distractor in the indexed library.
- **Aurora video:** [NASA / Crew Earth Observations, “Aurora Borealis over North America”](https://eol.jsc.nasa.gov/Videos/CrewEarthObservationsVideos/#borealis_iss_20120125), Expedition 30, January 25, 2012. [Original video](https://eol.jsc.nasa.gov/Videos/CrewEarthObservationsVideos/videos/borealis_iss_20120125/borealis_iss_20120125HD_web.mp4). The sample is a silent seven-second excerpt starting at 4.6 seconds, after the introductory title card. NASA imagery; no endorsement is implied.
- **GNOME Adwaita background:** © 2026 Jakub Steiner, [CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/). [Original background and embedded attribution](https://github.com/GNOME/gnome-backgrounds/blob/main/backgrounds/adwaita-d.jxl). Resized for the demonstration desktop.
- **Documents and notes:** fictional examples created for this demonstration. They contain no user documents or personal information.

The demo recording, its preview/poster, and screenshots in `docs/screenshots/`
are distributed under **CC BY-SA 3.0**, retaining the credits above. The social
preview uses the project's existing GSeek icon and title card. GSeek's source
code and application assets retain their existing project licenses.
