"""Real D-Bus + TinySPARQL + EmbeddingGemma 2 contract tests.

The fixture endpoint uses GNOME's actual TinySPARQL library and Nepomuk ontology.
It provides deterministic discovery events, without depending on a user's files
or changing their running LocalSearch instance. Inference is never mocked.
"""
import concurrent.futures
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


def wait_for(predicate, timeout=90):
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
        settings = {"version": 1, "configured": False, "folders": [str(cls.docs)], "backend": os.environ.get("GSEEK_TEST_BACKEND", "cpu")}
        (ROOT / "config/gseek/settings.json").write_text(json.dumps(settings))
        cls.service_log = open(ROOT / "service.log", "w")
        subprocess.run([BINARY, "integrate"], check=True)
        # Exercise service activation through its installed XDG .service file, not a manually started daemon.
        cls.invoke("Configure", GLib.Variant("(s)", (json.dumps(settings),)))
        wait_for(lambda: cls.status()["ready"], timeout=300)
        wait_for(lambda: cls.status()["counts"]["pending"] == 0 and cls.status()["counts"]["files"] >= 5)

    @classmethod
    def tearDownClass(cls):
        cls.invoke("Command", GLib.Variant("(ss)", ("quit", "")))
        cls.loop.quit()
        cls.thread.join(5)
        cls.service_log.close()

    @classmethod
    def add(cls, path):
        uri = path.as_uri()
        cls.conn.update(f'INSERT DATA {{ GRAPH tracker:FileSystem {{ <{uri}> a nfo:FileDataObject ; nie:url {json.dumps(uri)} . }} }}', None)

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
        with sqlite3.connect(database) as db:
            before = db.execute("SELECT count(*) FROM chunks").fetchone()[0]
        old.rename(new)
        self.add(new)
        self.remove(old)
        wait_for(lambda: any(h["name"] == new.name for h in self.search("roasted tomato soup")))
        wait_for(lambda: self.status()["counts"]["pending"] == 0)
        with sqlite3.connect(database) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM chunks").fetchone()[0], before)
        new.unlink()
        self.remove(new)
        wait_for(lambda: all(h["name"] != new.name for h in self.search("roasted tomato soup")))

    def test_05_concurrent_queries_are_bounded_and_responsive(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            results = list(pool.map(self.search, ["lease pets", "train berlin"] * 8))
        self.assertTrue(any(results))
        self.assertTrue(self.status()["ready"])

    def test_06_exclusions_prune_previously_indexed_files(self):
        settings = json.loads(self.invoke("Settings")[0])
        settings["excluded_extensions"] = ["md"]
        self.invoke("Configure", GLib.Variant("(s)", (json.dumps(settings),)))
        wait_for(lambda: self.status()["ready"])
        self.assertEqual(self.search("INV-2026-0042")[0]["name"], "Rechnung.txt")

    def test_07_install_is_idempotent_and_uninstall_preserves_user_edits(self):
        subprocess.run([BINARY, "integrate"], check=True)
        desktop = ROOT / "data/applications/io.github.FarisZR.GSeek.desktop"
        self.assertIn(str(Path(BINARY).resolve()), desktop.read_text())
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

    def test_08_native_gtk_views(self):
        if os.environ.get("GSEEK_GUI_TEST_COMMAND"):
            subprocess.run(os.environ["GSEEK_GUI_TEST_COMMAND"].split(), check=True)
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
