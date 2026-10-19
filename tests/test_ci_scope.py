import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("ci_scope", ROOT / "scripts/ci-scope.py")
scope = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scope)

MANIFEST = '[package]\nversion="0.1.3"\n[dependencies]\nureq="3"\ngtk="0.11"\n'
LOCK = '''
[[package]]
name="gseek"
version="0.1.3"
dependencies=["gtk", "ureq"]
[[package]]
name="gtk"
version="0.11.5"
[[package]]
name="ureq"
version="3.4.2"
dependencies=["rustls"]
[[package]]
name="rustls"
version="0.23.0"
'''


class InferenceScope(unittest.TestCase):
    def selected(self, paths, manifest=MANIFEST, lock=LOCK):
        return scope.requires_inference(paths, MANIFEST, manifest, LOCK, lock)

    def test_ui_restart_icons_docs_and_workflow_changes_do_not_load_models(self):
        self.assertFalse(self.selected({"src/ui.rs", "src/lifecycle.rs", "src/main.rs", "src/integration.rs",
            "src/service.rs", "data/gseek.gresource.xml", "data/icons/check.svg", "README.md",
            "docs/DEVELOPMENT.md", "tests/e2e.py", ".github/workflows/ci.yml"}))

    def test_inference_code_input_readers_and_pins_require_the_full_suite(self):
        for path in scope.INFERENCE_PATHS:
            with self.subTest(path=path):
                self.assertTrue(self.selected({path}))

    def test_app_version_and_gtk_dependency_updates_do_not_load_models(self):
        self.assertFalse(self.selected({"Cargo.toml", "Cargo.lock"},
            MANIFEST.replace('version="0.1.3"', 'version="0.1.4"').replace('gtk="0.11"', 'gtk="0.12"'),
            LOCK.replace('version="0.1.3"', 'version="0.1.4"').replace('version="0.11.5"', 'version="0.12.0"')))

    def test_runtime_dependency_specification_change_requires_inference(self):
        self.assertTrue(self.selected({"Cargo.toml"}, MANIFEST.replace('ureq="3"', 'ureq="4"')))

    def test_transitive_runtime_dependency_update_requires_inference(self):
        self.assertTrue(self.selected({"Cargo.lock"}, lock=LOCK.replace('version="0.23.0"', 'version="0.24.0"')))

    def test_new_dependencies_default_to_runtime_coverage(self):
        self.assertTrue(self.selected({"Cargo.toml"}, MANIFEST + 'new_runtime_crate="1"\n'))

    def test_search_and_ui_changes_keep_fast_cpu_integration_coverage(self):
        for path in ["src/service.rs", "src/ui.rs", "src/store.rs", "tests/e2e.py", "Cargo.lock"]:
            self.assertTrue(scope.requires_search({path}, False))
        self.assertFalse(scope.requires_search({"README.md", ".github/workflows/ci.yml"}, False))
        self.assertTrue(scope.requires_search({"data/runtimes.json"}, True))


if __name__ == "__main__":
    unittest.main()
