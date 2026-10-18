"""Real D-Bus + TinySPARQL + EmbeddingGemma 2 contract tests.

The fixture endpoint uses GNOME's actual TinySPARQL library and Nepomuk ontology.
It provides deterministic discovery events, without depending on a user's files
or changing their running LocalSearch instance. Inference is never mocked.
"""
import concurrent.futures
from contextlib import closing
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import threading
import time
import unittest

import gi
try:
    gi.require_version("Tsparql", "3.0")
    from gi.repository import Tsparql as Sparql
except ValueError:
    gi.require_version("Tracker", "3.0")
    from gi.repository import Tracker as Sparql
from gi.repository import Gio, GLib

ROOT = Path(os.environ["GSEEK_TEST_ROOT"])
BINARY = os.environ["GSEEK_BINARY"]
BUS = "io.github.FarisZR.GSeek.Service"
CONTROL = "/io/github/FarisZR/GSeek/Service"
PROVIDER = "/io/github/FarisZR/GSeek/SearchProvider"
IFACE = "io.github.FarisZR.GSeek.Service1"
SHELL_IFACE = "org.gnome.Shell.SearchProvider2"


def wait_for(predicate, timeout=None):
    if timeout is None:
        # Software Vulkan on shared CI CPUs is much slower than native CPU inference.
        timeout = 300 if os.environ.get("GSEEK_TEST_BACKEND") == "vulkan" else 90
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            last = predicate()
            if last:
                return last
        except GLib.Error as error:
            last = str(error)
        time.sleep(0.05)
    raise AssertionError(f"Timed out waiting for condition; last result: {last}")


class EndToEnd(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        cls.conn = Sparql.SparqlConnection.new(
            Sparql.SparqlConnectionFlags.NONE, None,
            Sparql.sparql_get_ontology_nepomuk(), None)
        cls.endpoint = Sparql.EndpointDBus.new(cls.conn, cls.bus, None, None)
        cls.owner = Gio.bus_own_name_on_connection(
            cls.bus, "org.freedesktop.LocalSearch3", Gio.BusNameOwnerFlags.NONE, None, None)
        cls.loop = GLib.MainLoop()
        cls.thread = threading.Thread(target=cls.loop.run, daemon=True)
        cls.thread.start()
        cls.docs = ROOT / "Documents"
        cls.docs.mkdir()
        cls.files = {
            "Rental Agreement.txt": "Residential lease. Dogs and cats are welcome in this apartment. The tenant may keep household pets with written permission from the landlord.",
            "Rechnung.txt": "Rechnung INV-2026-0042. Die Zahlung für das Fahrrad beträgt 249 Euro. Bitte überweisen Sie den Betrag auf unser Bankkonto.",
            "ملاحظات.txt": "موعد الرحلة إلى برلين يوم السبت. تذكرة القطار وحجز الفندق في حقيبة السفر.",
            "Tomato Soup.md": "Recipe: roast tomatoes with garlic and olive oil, then blend into a creamy soup. Serve with bread and fresh basil.",
        }
        for name, content in cls.files.items():
            path = cls.docs / name
            path.write_text(content)
            cls.add(path)
        cls.add(cls.docs)
        settings = {"version": 1, "configured": False, "folders": [str(cls.docs)], "backend": os.environ.get("GSEEK_TEST_BACKEND", "cpu"), "search_enabled": True}
        (ROOT / "config/gseek/settings.json").write_text(json.dumps(settings))
        cls.service_log = open(ROOT / "service.log", "w")
        subprocess.run([BINARY, "integrate"], check=True)
        # Exercise service activation through its installed XDG .service file, not a manually started daemon.
        cls.invoke("Configure", GLib.Variant("(s)", (json.dumps(settings),)))
        wait_for(lambda: cls.status()["ready"], timeout=300)
        try:
            wait_for(lambda: cls.status()["counts"]["pending"] == 0 and cls.status()["counts"]["files"] >= 5)
        except AssertionError as error:
            raise AssertionError(f"Initial indexing did not finish: {cls.status()}") from error

    @classmethod
    def tearDownClass(cls):
        cls.invoke("Command", GLib.Variant("(ss)", ("quit", "")))
        cls.loop.quit()
        cls.thread.join(5)
        cls.service_log.close()

    @classmethod
    def add(cls, path):
        uri = path.as_uri()
        cls.conn.update(f'INSERT DATA {{ GRAPH tracker:FileSystem {{ <{uri}> a nfo:FileDataObject ; nie:url {json.dumps(uri)} ; nfo:fileName {json.dumps(path.name, ensure_ascii=False)} . }} }}', None)

    @classmethod
    def remove(cls, path):
        cls.conn.update(f'DELETE WHERE {{ GRAPH tracker:FileSystem {{ <{path.as_uri()}> ?p ?o }} }}', None)

    @classmethod
    def invoke(cls, method, parameters=None, interface=IFACE, path=CONTROL):
        return cls.bus.call_sync(BUS, path, interface, method, parameters, None, Gio.DBusCallFlags.NONE, 120000, None).unpack()

    @classmethod
    def status(cls):
        return json.loads(cls.invoke("Status")[0])

    @classmethod
    def search(cls, query):
        return json.loads(cls.invoke("Search", GLib.Variant("(su)", (query, 100)))[0])

    def command(self, command, argument=""):
        self.invoke("Command", GLib.Variant("(ss)", (command, argument)))

    def test_01_real_multilingual_hybrid_retrieval(self):
        for query, expected in [
            ("the clause about pets in my lease", "Rental Agreement.txt"),
            ("Haustiere in der Wohnung erlaubt", "Rental Agreement.txt"),
            ("فاتورة شراء الدراجة", "Rechnung.txt"),
            ("train journey to Berlin", "ملاحظات.txt"),
            ("INV-2026-0042", "Rechnung.txt"),
        ]:
            with self.subTest(query=query):
                hits = self.search(query)
                self.assertTrue(hits, query)
                self.assertEqual(hits[0]["name"], expected, hits)

    def test_02_shell_contract_and_native_icons(self):
        ids = self.invoke("GetInitialResultSet", GLib.Variant("(as)", (["pets", "lease"],)), SHELL_IFACE, PROVIDER)[0]
        self.assertTrue(ids)
        metas = self.invoke("GetResultMetas", GLib.Variant("(as)", (ids[:3],)), SHELL_IFACE, PROVIDER)[0]
        self.assertEqual(metas[0]["id"], ids[0])
        self.assertIn("icon", metas[0])
        refined = self.invoke("GetSubsearchResultSet", GLib.Variant("(asas)", (ids, ["pets", "in", "lease"])), SHELL_IFACE, PROVIDER)[0]
        self.assertTrue(refined)

    def test_02b_activating_a_result_uses_its_default_application(self):
        helper=ROOT / "capture-open.py"
        output=ROOT / "opened.json"
        helper.write_text("import json,sys\nfrom pathlib import Path\n" + f"Path({str(output)!r}).write_text(json.dumps(sys.argv[1:]))\n")
        app=Gio.AppInfo.create_from_commandline(f'{sys.executable} "{helper}" %u',"GSeek test reader",Gio.AppInfoCreateFlags.SUPPORTS_URIS)
        app.set_as_default_for_type("text/plain")
        uri=(self.docs / "Rental Agreement.txt").as_uri()
        self.invoke("ActivateResult",GLib.Variant("(sasu)",(uri,["pets"],0)),SHELL_IFACE,PROVIDER)
        wait_for(output.exists)
        self.assertTrue(any(Gio.File.new_for_commandline_arg(arg).equal(Gio.File.new_for_uri(uri)) for arg in json.loads(output.read_text())))

    def test_02c_gnome_filename_results_do_not_embed_downloads_or_unsupported_files(self):
        downloads = ROOT / "Downloads"
        downloads.mkdir(exist_ok=True)
        visible = [downloads / "Boarding Pass العربية.txt", downloads / "Backup Archive.zip"]
        hidden = [downloads / ".Boarding Hidden.txt", downloads / "Boarding Key.pem"]
        for path in visible + hidden:
            path.write_text("This content must never enter the embedding queue.")
            self.add(path)
        try:
            hits = self.search("Pass العربية")
            self.assertTrue(any(hit["uri"] == visible[0].as_uri() for hit in hits), hits)
            self.assertTrue(any(hit["uri"] == visible[1].as_uri() for hit in self.search("Backup Archive")))
            ids = self.invoke("GetInitialResultSet", GLib.Variant("(as)", (["Boarding", "Pass"],)), SHELL_IFACE, PROVIDER)[0]
            self.assertIn(visible[0].as_uri(), ids)
            metas = self.invoke("GetResultMetas", GLib.Variant("(as)", ([visible[0].as_uri()],)), SHELL_IFACE, PROVIDER)[0]
            self.assertEqual(metas[0]["name"], visible[0].name)
            self.assertIn("icon", metas[0])
            self.assertFalse(any(hit["uri"] in [p.as_uri() for p in hidden] for hit in self.search("Boarding")))
            with closing(sqlite3.connect(ROOT / "data/gseek/index.sqlite3")) as db:
                for path in visible + hidden:
                    self.assertIsNone(db.execute("SELECT uri FROM files WHERE uri=?", (path.as_uri(),)).fetchone())
            # An ordinary filename result must open with the same Shell action.
            output = ROOT / "opened.json"
            output.unlink(missing_ok=True)
            self.invoke("ActivateResult", GLib.Variant("(sasu)", (visible[0].as_uri(), ["Boarding"], 0)), SHELL_IFACE, PROVIDER)
            wait_for(output.exists)
            self.assertTrue(any(Gio.File.new_for_commandline_arg(arg).equal(Gio.File.new_for_path(str(visible[0]))) for arg in json.loads(output.read_text())))
            self.remove(visible[0])
            self.assertFalse(any(hit["uri"] == visible[0].as_uri() for hit in self.search("Pass العربية")))
        finally:
            for path in visible + hidden:
                self.remove(path)
                path.unlink()

    def test_02c_real_gnome_overview_loads_and_queries_the_provider(self):
        if not os.environ.get("GSEEK_TEST_SHELL"):
            self.skipTest("Set GSEEK_TEST_SHELL=1 with the keyfile settings backend for a private headless GNOME session")
        from shell import check_overview
        check_overview(self.bus, ROOT, wait_for)

    def test_03_pause_persists_and_resume_processes_new_files(self):
        self.command("pause")
        self.assertTrue(self.status()["paused"])
        path = self.docs / "New Meeting.txt"
        path.write_text("We are planning a hiking expedition through the Black Forest next spring.")
        self.add(path)
        wait_for(lambda: self.status()["counts"]["pending"] >= 1)
        self.assertTrue(json.loads((ROOT / "config/gseek/settings.json").read_text())["paused"])
        self.command("resume")
        wait_for(lambda: self.status()["counts"]["pending"] == 0)
        self.assertEqual(self.search("hiking expedition")[0]["name"], path.name)

    def test_04_rename_reuses_vectors_and_delete_disappears(self):
        old = self.docs / "Tomato Soup.md"
        new = self.docs / "Dinner Recipe.md"
        database = ROOT / "data/gseek/index.sqlite3"
        with closing(sqlite3.connect(database)) as db:
            before = db.execute("SELECT count(*) FROM chunks").fetchone()[0]
        old.rename(new)
        self.add(new)
        self.remove(old)
        wait_for(lambda: any(h["name"] == new.name for h in self.search("roasted tomato soup")))
        wait_for(lambda: self.status()["counts"]["pending"] == 0)
        with closing(sqlite3.connect(database)) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM chunks").fetchone()[0], before)
        new.unlink()
        self.remove(new)
        wait_for(lambda: all(h["name"] != new.name for h in self.search("roasted tomato soup")))

    def test_05_concurrent_queries_are_bounded_and_responsive(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            results = list(pool.map(self.search, ["lease pets", "train berlin"] * 8))
        self.assertTrue(any(results))
        self.assertTrue(self.status()["ready"])

    def test_05b_large_jobs_yield_and_resume_checkpoints_after_service_restart(self):
        large = self.docs / "Observatory Archive.txt"
        small = self.docs / "Urgent Appointment.txt"
        database = ROOT / "data/gseek/index.sqlite3"
        large.write_text("The coastal observatory records tides, migrating birds, and rainfall every morning. " * 5000)

        def partial():
            with closing(sqlite3.connect(database)) as db:
                return db.execute("SELECT contents.id,count(chunks.id) FROM contents JOIN chunks ON chunks.content=contents.id WHERE complete=0 GROUP BY contents.id HAVING count(chunks.id)>0").fetchone()

        def file_state(path):
            with closing(sqlite3.connect(database)) as db:
                row = db.execute("SELECT state FROM files WHERE uri=?", (path.as_uri(),)).fetchone()
                return row[0] if row else None

        try:
            self.add(large)
            wait_for(partial)
            small.write_text("Urgent appointment: collect the new telescope from the astronomy shop tomorrow.")
            self.add(small)
            wait_for(lambda: file_state(small) == "indexed")
            self.assertEqual(file_state(large), "pending", "The smaller new file must finish before the long archive")
            self.assertEqual(self.search("collect the new telescope")[0]["name"], small.name)
            self.command("pause")
            content, completed = partial()
            self.assertGreater(completed, 0)
            self.command("quit")
            wait_for(lambda: not self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner", GLib.Variant("(s)", (BUS,)), None, Gio.DBusCallFlags.NONE, 1000, None).unpack()[0])
            wait_for(lambda: self.status()["ready"], timeout=300)
            self.assertTrue(self.status()["paused"])
            self.assertGreaterEqual(partial()[1], completed)
            self.command("resume")
            def resumed():
                rows = json.loads(self.invoke("Activity")[0])
                return any(row["state"] == "resumed" and large.as_uri() == row["path"] for row in rows)
            wait_for(resumed)
            self.assertEqual(partial()[0], content)
        finally:
            for path in [large, small]:
                path.unlink(missing_ok=True)
                self.remove(path)
            self.command("resume")
            wait_for(lambda: self.status()["counts"]["pending"] == 0)

    def test_05c_dense_input_finishes_without_stopping_indexing(self):
        dense = self.docs / "Dense Input.txt"
        # Reproduce the 2,048-token boundary without duplicating the much larger
        # multilingual/long-query stress test in real_inference.rs.
        dense.write_text("1234567890" * 230)
        self.add(dense)
        def indexed():
            with closing(sqlite3.connect(ROOT / "data/gseek/index.sqlite3")) as db:
                row = db.execute("SELECT state FROM files WHERE uri=?", (dense.as_uri(),)).fetchone()
                return row and row[0] == "indexed"
        try:
            # This file requires two bounded embedding calls. The software Vulkan
            # driver can legitimately need most of the per-call inference timeout.
            wait_for(indexed, timeout=600 if os.environ.get("GSEEK_TEST_BACKEND") == "vulkan" else 90)
            self.assertTrue(self.status()["ready"])
            self.assertEqual(self.search("Dense Input.txt")[0]["uri"], dense.as_uri())
        finally:
            self.remove(dense)
            dense.unlink()

    def test_05d_filename_lookup_failure_keeps_existing_semantic_results(self):
        def reject_filename_query(connection, message, incoming, _data):
            body = message.get_body()
            if incoming and message.get_member() == "Query" and body and "fts:match" in str(body):
                reply = message.new_method_error_literal("org.freedesktop.DBus.Error.Failed", "Filename lookup interrupted for recovery test")
                connection.send_message(reply, Gio.DBusSendMessageFlags.NONE)
                return None
            return message
        filter_id = self.bus.add_filter(reject_filename_query, None)
        try:
            self.assertEqual(self.search("the clause about pets in my lease")[0]["name"], "Rental Agreement.txt")
            self.assertIn("Filename lookup interrupted", self.status()["filename_error"])
            self.assertTrue(self.status()["ready"])
        finally:
            self.bus.remove_filter(filter_id)
        self.search("Rental Agreement")
        self.assertEqual(self.status()["filename_error"], "")

    def test_05e_filename_paging_skips_excluded_candidates_before_limiting(self):
        directory = ROOT / "Paged Results"
        directory.mkdir()
        hidden = [directory / f".Paging Item {i:03}.txt" for i in range(520)]
        visible = directory / "Paging Item Visible.txt"
        visible.write_text("This content is outside embedding scope.")
        entries = []
        for path in hidden + [visible]:
            entries.append(f'<{path.as_uri()}> a nfo:FileDataObject ; nie:url {json.dumps(path.as_uri())} ; nfo:fileName {json.dumps(path.name)} .')
        self.conn.update('INSERT DATA { GRAPH tracker:FileSystem { ' + ' '.join(entries) + ' } }', None)
        try:
            hits = self.search("Paging Item")
            self.assertEqual([hit["uri"] for hit in hits if "Paging Item" in hit["name"]], [visible.as_uri()])
        finally:
            self.conn.update('DELETE { GRAPH tracker:FileSystem { ?s ?p ?o } } WHERE { GRAPH tracker:FileSystem { ?s ?p ?o . FILTER (STRSTARTS(STR(?s), ' + json.dumps(directory.as_uri() + '/') + ')) } }', None)
            visible.unlink()
            directory.rmdir()

    def test_06_exclusions_prune_previously_indexed_files(self):
        note = self.docs / "Excluded Note.md"
        note.write_text("A handmade ceramic teapot with a green glaze.")
        database = ROOT / "data/gseek/index.sqlite3"
        def note_state():
            with closing(sqlite3.connect(database)) as db:
                row = db.execute("SELECT state FROM files WHERE uri=?", (note.as_uri(),)).fetchone()
                return row[0] if row else None
        self.add(note)
        wait_for(lambda: note_state() == "indexed")
        wait_for(lambda: any(hit["name"] == note.name for hit in self.search("Excluded Note.md")))
        settings = json.loads(self.invoke("Settings")[0])
        settings["excluded_extensions"] = ["md"]
        self.invoke("Configure", GLib.Variant("(s)", (json.dumps(settings),)))
        wait_for(lambda: self.status()["ready"])
        wait_for(lambda: note_state() is None)
        self.assertFalse(any(hit["name"] == note.name for hit in self.search("Excluded Note.md")))
        self.assertEqual(self.search("INV-2026-0042")[0]["name"], "Rechnung.txt")

    def test_06a_folder_exclusion_deletes_index_and_queue_immediately(self):
        excluded = self.docs / "Private Archive"
        excluded.mkdir()
        indexed = excluded / "Indexed.txt"
        queued = excluded / "Queued.txt"
        indexed.write_text("A private archive of handmade wooden chess sets.")
        database = ROOT / "data/gseek/index.sqlite3"
        def row(path):
            with closing(sqlite3.connect(database)) as db:
                return db.execute("SELECT state,content FROM files WHERE uri=?", (path.as_uri(),)).fetchone()
        previous = json.loads(self.invoke("Settings")[0])["excluded_folders"]
        try:
            self.add(indexed)
            wait_for(lambda: row(indexed) and row(indexed)[0] == "indexed")
            content = row(indexed)[1]
            self.command("pause")
            queued.write_text("A pending document that must never be indexed after exclusion.")
            self.add(queued)
            wait_for(lambda: row(queued) is not None)
            self.invoke("UpdateSettings", GLib.Variant("(s)", (json.dumps({"excluded_folders": previous + [str(excluded)]}),)))
            self.assertIsNone(row(indexed), "Exclusion must be committed before the preference call returns")
            self.assertIsNone(row(queued))
            with closing(sqlite3.connect(database)) as db:
                self.assertEqual(db.execute("SELECT count(*) FROM chunks WHERE content=?", (content,)).fetchone()[0], 0)
            self.command("scan")
            self.command("resume")
            wait_for(lambda: self.status()["ready"])
            self.assertIsNone(row(indexed))
            self.assertIsNone(row(queued))
            self.assertFalse(any(hit["uri"] == indexed.as_uri() for hit in self.search("handmade wooden chess sets")))
        finally:
            for path in [indexed, queued]:
                path.unlink(missing_ok=True)
                self.remove(path)
            excluded.rmdir()
            self.invoke("UpdateSettings", GLib.Variant("(s)", (json.dumps({"excluded_folders": previous}),)))
            self.command("resume")
            wait_for(lambda: self.status()["ready"])

    def test_06b_enabling_images_reconsiders_previously_skipped_files(self):
        settings = json.loads(self.invoke("Settings")[0])
        settings["images"] = False
        self.invoke("Configure", GLib.Variant("(s)", (json.dumps(settings),)))
        wait_for(lambda: self.status()["ready"])
        picture = self.docs / "Blue Square.ppm"
        picture.write_bytes(b"P6\n128 128\n255\n" + bytes([10, 60, 240]) * 128 * 128)
        database = ROOT / "data/gseek/index.sqlite3"

        def state():
            with closing(sqlite3.connect(database)) as db:
                row = db.execute("SELECT state FROM files WHERE uri=?", (picture.as_uri(),)).fetchone()
                return row[0] if row else None

        try:
            self.add(picture)
            wait_for(lambda: state() == "skipped")
            settings["images"] = True
            self.invoke("Configure", GLib.Variant("(s)", (json.dumps(settings),)))
            try:
                wait_for(lambda: state() == "indexed", timeout=300)
            except AssertionError as error:
                raise AssertionError(f"Image indexing did not finish: file={state()}, service={self.status()}") from error
        finally:
            picture.unlink(missing_ok=True)
            self.remove(picture)
            wait_for(lambda: state() is None)

    def test_06c_stalled_cuda_is_cancelled_and_failed_cuda_keeps_the_saved_choice(self):
        # Fault injection tests process cancellation, not CUDA inference. Healthy
        # embeddings still come from the real upstream CPU/Vulkan distribution.
        runtime_root = ROOT / "controlled-runtimes"
        runtime_root.mkdir()
        cache = ROOT / "data/gseek/runtimes"
        for backend in ["cpu", "vulkan"]:
            directories = list(cache.glob(f"*-{backend}-*"))
            if directories:
                (runtime_root / backend).symlink_to(directories[0].resolve(), target_is_directory=True)
        cuda = runtime_root / "cuda"
        cuda.mkdir()
        pidfile = ROOT / "cuda.pid"
        fail = ROOT / "cuda-fail"
        engine = cuda / "llama-server"
        engine.write_text(f"#!{sys.executable}\nimport os,sys,time\nfrom pathlib import Path\nPath({str(pidfile)!r}).write_text(str(os.getpid()))\nif Path({str(fail)!r}).exists():\n sys.stderr.write('CUDA error: out of memory\\n')\n sys.exit(1)\nwhile True: time.sleep(1)\n")
        engine.chmod(0o700)
        original = json.loads(self.invoke("Settings")[0])["backend"]
        restore = "auto" if original == "cpu" else original
        def activation(value):
            self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "UpdateActivationEnvironment", GLib.Variant("(a{ss})", ({"GSEEK_RUNTIME_DIR": value},)), None, Gio.DBusCallFlags.NONE, 1000, None)
        def stopped():
            return not self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner", GLib.Variant("(s)", (BUS,)), None, Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
        def update(backend):
            self.invoke("UpdateSettings", GLib.Variant("(s)", (json.dumps({"backend": backend}),)))
        probe = self.docs / "Backend Recovery.txt"
        try:
            activation(str(runtime_root))
            self.command("quit")
            wait_for(stopped)
            wait_for(lambda: self.status()["ready"], timeout=300)
            update("cuda")
            wait_for(pidfile.exists, timeout=5)
            pid = int(pidfile.read_text())
            self.assertEqual(json.loads(self.invoke("Settings")[0])["backend"], "cuda")
            self.assertEqual(self.status()["requested_backend"], "cuda")
            update(restore)
            wait_for(lambda: not Path(f"/proc/{pid}").exists(), timeout=5)
            wait_for(lambda: self.status()["ready"], timeout=300)
            if restore == "auto":
                self.assertEqual(self.status()["backend"], "CPU")
            else:
                self.assertIn("Vulkan · Software CPU · llvmpipe", self.status()["backend"])
            probe.write_text("A recovered indexing job records the silver telescope lens.")
            self.add(probe)
            wait_for(lambda: any(hit["uri"] == probe.as_uri() for hit in self.search("silver telescope lens")))
            pidfile.unlink()
            fail.touch()
            update("cuda")
            wait_for(lambda: self.status()["state"] == "error", timeout=10)
            self.assertIn("out of memory", self.status()["detail"])
            self.assertEqual(json.loads(self.invoke("Settings")[0])["backend"], "cuda")
            update(restore)
            wait_for(lambda: self.status()["ready"], timeout=300)
        finally:
            probe.unlink(missing_ok=True)
            self.remove(probe)
            update(original)
            wait_for(lambda: self.status()["ready"], timeout=300)
            activation(os.environ.get("GSEEK_RUNTIME_DIR", ""))
            self.command("quit")
            wait_for(stopped)
            wait_for(lambda: self.status()["ready"], timeout=300)

    def test_07_install_is_idempotent_and_uninstall_preserves_user_edits(self):
        subprocess.run([BINARY, "integrate"], check=True)
        desktop = ROOT / "data/applications/io.github.FarisZR.GSeek.desktop"
        self.assertIn(str(Path(BINARY).resolve()), desktop.read_text())
        icon = Path(next(line[5:] for line in desktop.read_text().splitlines() if line.startswith("Icon=")))
        self.assertTrue(icon.is_absolute(), "Desktop icon must not depend on theme cache lookup")
        self.assertEqual(icon.read_bytes()[:8], b"\x89PNG\r\n\x1a\n")
        # The complete lifecycle is checked in a separate sandbox so it cannot stop this test service.
        root = ROOT / "install-test"
        env = {**os.environ, "XDG_DATA_HOME": str(root / "data"), "XDG_CONFIG_HOME": str(root / "config"), "XDG_CACHE_HOME": str(root / "cache")}
        subprocess.run([BINARY, "integrate"], env=env, check=True)
        desktop = root / "data/applications/io.github.FarisZR.GSeek.desktop"
        desktop.write_text(desktop.read_text() + "# User customization\n")
        manifest = json.loads((root / "config/gseek/integration.json").read_text())
        self.assertIn(str(desktop), manifest["files"])
        sentinel=root / "data/gseek/preserve.txt"
        sentinel.write_text("Keep indexed data")
        subprocess.run([BINARY,"unintegrate"],env=env,check=True)
        self.assertTrue(desktop.is_file(),"User-edited registration must be preserved")
        self.assertTrue(sentinel.is_file())
        self.assertFalse((root / "data/dbus-1/services/io.github.FarisZR.GSeek.Service.service").exists())
        wait_for(lambda:self.status()["ready"])

    def test_07b_show_all_launches_and_updates_the_existing_native_window(self):
        if not os.environ.get("DISPLAY"):
            self.skipTest("Needs a native display")
        def running():
            return self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner", GLib.Variant("(s)", ("io.github.FarisZR.GSeek",)), None, Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]
        self.invoke("LaunchSearch", GLib.Variant("(asu)", (["Rental", "Agreement"], 0)), SHELL_IFACE, PROVIDER)
        wait_for(running)
        actions = Gio.DBusActionGroup.get(self.bus, "io.github.FarisZR.GSeek", "/io/github/FarisZR/GSeek")
        def query():
            state = actions.get_action_state("search")
            return state.unpack() if state else None
        try:
            wait_for(lambda: query() == "Rental Agreement")
            self.invoke("LaunchSearch", GLib.Variant("(asu)", (["Tomato", "Soup"], 0)), SHELL_IFACE, PROVIDER)
            wait_for(lambda: query() == "Tomato Soup")
            for literal in ["-report", "--help", "--"]:
                self.invoke("LaunchSearch", GLib.Variant("(asu)", ([literal], 0)), SHELL_IFACE, PROVIDER)
                wait_for(lambda: query() == literal)
        finally:
            actions.activate_action("quit", None)
            wait_for(lambda: not running())

    def test_07c_restart_menu_and_install_replace_the_live_window_and_service(self):
        if not os.environ.get("DISPLAY") and not os.environ.get("WAYLAND_DISPLAY"):
            self.skipTest("Needs a native display")
        app_id = "io.github.FarisZR.GSeek"
        def owner(name):
            try:
                return self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                    "org.freedesktop.DBus", "GetNameOwner", GLib.Variant("(s)", (name,)),
                    None, Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
            except GLib.Error:
                return None
        def activate(action):
            self.bus.call_sync(app_id, "/io/github/FarisZR/GSeek", "org.gtk.Actions", "Activate",
                GLib.Variant("(sava{sv})", (action, [], {})), None, Gio.DBusCallFlags.NONE, 2000, None)
        def replaced(name, previous):
            current = owner(name)
            return current if current and current != previous else None
        initial_window = subprocess.Popen([BINARY], stdout=self.service_log, stderr=self.service_log)
        wait_for(lambda: owner(app_id), timeout=10)
        try:
            for restart in [lambda: activate("restart"),
                            lambda: subprocess.run([BINARY, "integrate"], check=True, timeout=40)]:
                previous_window, previous_service = owner(app_id), owner(BUS)
                settings = self.invoke("Settings")[0]
                restart()
                wait_for(lambda: replaced(app_id, previous_window), timeout=15)
                wait_for(lambda: replaced(BUS, previous_service), timeout=15)
                self.assertEqual(self.invoke("Settings")[0], settings)
                expected_version = subprocess.check_output([BINARY, "--version"], text=True).strip().split()[1]
                self.assertEqual(self.status()["version"], expected_version)
                wait_for(lambda: self.status()["ready"], timeout=300)
                self.assertTrue(self.search("pets lease"))
        finally:
            if owner(app_id):
                activate("quit")
                wait_for(lambda: not owner(app_id), timeout=10)
            initial_window.wait(timeout=10)

    def test_08_native_gtk_views(self):
        if os.environ.get("GSEEK_GUI_TEST_COMMAND"):
            downloads = ROOT / "Downloads"
            downloads.mkdir(exist_ok=True)
            files = [downloads / f"Show All Results {i}.txt" for i in range(7)]
            for path in files:
                path.write_text("Only the filename should be searched.")
                self.add(path)
            slow_open_seen = threading.Event()
            def slow_open_query(_connection, message, incoming, _data):
                body = message.get_body()
                if incoming and message.get_member() == "Query" and body and "?url = " in str(body) and "Show%20All%20Results%200.txt" in str(body):
                    slow_open_seen.set()
                    time.sleep(1.2)
                return message
            filter_id = self.bus.add_filter(slow_open_query, None)
            try:
                wait_for(lambda: len([hit for hit in self.search("Show All Results") if hit["name"].startswith("Show All Results")]) == 7)
                subprocess.run(os.environ["GSEEK_GUI_TEST_COMMAND"].split(), check=True)
                self.assertTrue(slow_open_seen.is_set(), "The native file opener must exercise the delayed filename lookup")
            finally:
                self.bus.remove_filter(filter_id)
                for path in files:
                    self.remove(path)
                    path.unlink()
            self.assertTrue(self.status()["ready"],"Closing the GTK window must not stop the background service")
        else:
            self.skipTest("Run under Xvfb or GNOME with GSEEK_GUI_TEST_COMMAND to capture native views")

    def test_09_incompatible_inference_shows_error_without_keyword_fallback(self):
        settings = json.loads(self.invoke("Settings")[0])
        settings["external"] = {"url": "http://127.0.0.1:9/v1", "model": "embeddinggemma-2", "allow_remote": False}
        self.invoke("Configure", GLib.Variant("(s)", (json.dumps(settings),)))
        wait_for(lambda: self.status()["state"] == "error")
        with self.assertRaises(GLib.Error):
            self.search("Rental Agreement")
        self.assertIn("connect", self.status()["detail"].lower())


if __name__ == "__main__":
    unittest.main(verbosity=2)
