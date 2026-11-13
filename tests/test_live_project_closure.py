"""Harness tests for tools/live_project_closure.py (project-work closure).

None of these fixtures are cross-host evidence. They pin the acceptance
checker's stable-selector contract from the design document: mount markers
(data-search-input / data-category-list / data-results-list) must sit in
the static index.html text, while renderer markers ([data-card]) may be
produced by the referenced JavaScript at runtime — either as an HTML
string literal ("data-card") or via the DOM dataset API (dataset.card,
camelCase). Regressions this file pins:

- static index.html check rejecting a runtime-only mount marker (run
  ea3aed16 regression: the checker used to demand every marker in
  index.html text, so a compliant artifact whose renderer writes
  attributes via dataset.* was rejected with
  stable_selector_missing:data-card).
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DRIVER = ROOT / "tools" / "live_project_closure.py"


def load_driver():
    spec = importlib.util.spec_from_file_location("live_project_closure", DRIVER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _marker_page(body: str) -> tuple[bytes, str]:
    """Minimal artifact page satisfying the design contract outside markers."""
    html = (
        "<!doctype html><html><head><meta charset=\"utf-8\">"
        "<title>成都遛娃小工具</title></head><body>"
        "<input data-search-input><nav data-category-list></nav>"
        "<main data-results-list>" + body + "</main>"
        "<script src=\"js/app.js\"></script></body></html>"
    )
    return html.encode("utf-8"), "text/html"


def _seed_store(root: Path, files: dict) -> tuple[Path, str, str]:
    """Seed a passing project store (controller.sqlite) + artifact dir.

    Returns (database path, document sha256, run id). Mirrors the shape
    inspect_project_store requires: one done run, admitted owner,
    released assignment, model evidence, artifact package with sha256
    chain, artifact_dir annotation.
    """
    document = "设计文档正文".encode("utf-8")
    document_sha = hashlib.sha256(document).hexdigest()
    artifact_dir = root / "artifacts" / "run-1"
    artifact_dir.mkdir(parents=True)
    package_files = []
    for name, raw in files.items():
        target = artifact_dir / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(raw)
        package_files.append({"path": name, "size": len(raw),
                              "sha256": hashlib.sha256(raw).hexdigest(),
                              "content_hex": raw.hex()})
    report = {"schema": "native-project/1", "session": "s1",
              "document_sha256": document_sha,
              "model": {"model": "m", "request_sha256": "r" * 64,
                        "response_sha256": "p" * 64, "elapsed_ms": 1,
                        "status": 200}}
    run = {"id": "run-1", "attempt": 1, "template": "project-work-v1",
           "status": "done", "session": "s1",
           "detail": {"owner_admission": "admitted", "report": report,
                      "artifact": {"schema": "project-artifact-package/1",
                                   "document_sha256": document_sha,
                                   "files": package_files},
                      "artifact_dir": str(artifact_dir)}}
    database = root / "controller.sqlite"
    connection = sqlite3.connect(database)
    connection.executescript(
        "CREATE TABLE metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);"
        "CREATE TABLE runs(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT,body TEXT NOT NULL,status TEXT NOT NULL,attempt INTEGER NOT NULL);"
        "CREATE TABLE assignments(run TEXT NOT NULL,attempt INTEGER NOT NULL,node TEXT NOT NULL,address TEXT NOT NULL,released INTEGER NOT NULL);"
    )
    connection.execute("INSERT INTO metadata VALUES('deployment','deploy-1')")
    connection.execute("INSERT INTO runs(id,body,status,attempt) VALUES(?,?,?,?)",
                       (run["id"], json.dumps(run), "done", 1))
    connection.execute("INSERT INTO assignments VALUES('run-1',1,'B','addr',1)")
    connection.commit()
    connection.close()
    return database, document_sha, run["id"]


def _artifact_files(js_body: str, *, html_markers: bool = True) -> dict:
    html_raw, _ = _marker_page("" if html_markers else "")
    if not html_markers:
        # strip the mount markers to build the negative fixture
        html_raw = html_raw.replace(b"data-search-input", b"search-input")
        html_raw = html_raw.replace(b"data-category-list", b"category-list")
        html_raw = html_raw.replace(b"data-results-list", b"results-list")
    return {"index.html": html_raw, "js/app.js": js_body.encode("utf-8")}


class StableSelectorTests(unittest.TestCase):
    def setUp(self):
        self.driver = load_driver()

    def test_dom_api_renderer_marker_is_accepted(self):
        """dataset.card (camelCase DOM API) satisfies [data-card]."""
        js = "var a=document.createElement('article');a.dataset.card='';a.dataset.id=x;"
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            database, document_sha, _ = _seed_store(root, _artifact_files(js))
            facts = self.driver.inspect_project_store(
                database, "deploy-1", "B", "a" * 64, document_sha, root, token=b"synthetic-token-not-in-fixtures")
        self.assertEqual(facts.get("status"), "PASS_PROJECT_STORE_ADMISSION", facts)

    def test_html_string_renderer_marker_is_accepted(self):
        """A literal 'data-card' string in the referenced js also satisfies it."""
        js = "list.innerHTML='<article data-card data-id=\"1\"></article>';"
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            database, document_sha, _ = _seed_store(root, _artifact_files(js))
            facts = self.driver.inspect_project_store(
                database, "deploy-1", "B", "a" * 64, document_sha, root, token=b"synthetic-token-not-in-fixtures")
        self.assertEqual(facts.get("status"), "PASS_PROJECT_STORE_ADMISSION", facts)

    def test_missing_renderer_marker_is_still_rejected(self):
        """No marker anywhere (html or js) must stay NOT_ADMITTED."""
        js = "var a=document.createElement('article');a.className='card';"
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            database, document_sha, _ = _seed_store(root, _artifact_files(js))
            facts = self.driver.inspect_project_store(
                database, "deploy-1", "B", "a" * 64, document_sha, root, token=b"synthetic-token-not-in-fixtures")
        self.assertEqual(facts.get("status"), "NOT_ADMITTED", facts)
        self.assertEqual(facts.get("reason"), "stable_selector_missing:data-card", facts)

    def test_mount_markers_must_stay_in_static_index_html(self):
        """Mount points cannot migrate into js; static html check remains."""
        js = "var a=document.createElement('article');a.dataset.card='';"
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            database, document_sha, _ = _seed_store(
                root, _artifact_files(js, html_markers=False))
            facts = self.driver.inspect_project_store(
                database, "deploy-1", "B", "a" * 64, document_sha, root, token=b"synthetic-token-not-in-fixtures")
        self.assertEqual(facts.get("status"), "NOT_ADMITTED", facts)
        self.assertIn("stable_selector_missing:data-search-input", facts.get("reason", ""))


class ManagerDiagSurvivalTests(unittest.TestCase):
    """Gate: append_diag fix must keep all three events alive in a session.

    Pins the -44 real-session contract (three lines) as a harness check so a
    regression fails the closure run instead of silently dropping lines.
    """

    def setUp(self):
        self.driver = load_driver()

    @staticmethod
    def _diag(*lines: str) -> str:
        return "".join(line + "\n" for line in lines)

    def test_all_three_events_pass(self):
        """gateway_configured + dispatch_result(done,run) + artifact_materialized(run)."""
        log = self._diag(
            "1794520391\tgateway_configured\tworker=B artifact=9138e82e",
            "1794520491\tdispatch_result\trun_id=run-1 status=done project=doc-project",
            "1794520491\tartifact_materialized\trun_id=run-1 files=4",
        )
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "manager-diag.log").write_text(log, encoding="utf-8")
            facts = self.driver.inspect_manager_diag(root, "run-1")
        self.assertEqual(facts.get("status"), "PASS_MANAGER_DIAG_SURVIVAL", facts)

    def test_missing_file_rejected(self):
        """No manager-diag.log at all → NOT_CONFIRMED (pre-fix sessions had none)."""
        with tempfile.TemporaryDirectory() as temp:
            facts = self.driver.inspect_manager_diag(Path(temp), "run-1")
        self.assertEqual(facts.get("status"), "NOT_CONFIRMED", facts)
        self.assertEqual(facts.get("reason"), "manager_diag_missing", facts)

    def test_partial_log_rejected(self):
        """Only artifact_materialized (the -41/-43 dropped-lines shape) fails."""
        log = self._diag("1794520491\tartifact_materialized\trun_id=run-1 files=4")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "manager-diag.log").write_text(log, encoding="utf-8")
            facts = self.driver.inspect_manager_diag(root, "run-1")
        self.assertEqual(facts.get("status"), "NOT_CONFIRMED", facts)
        self.assertEqual(facts.get("reason"), "manager_diag_event_missing:gateway_configured", facts)

    def test_foreign_run_id_does_not_count(self):
        """dispatch_result for another run must not satisfy this run's gate."""
        log = self._diag(
            "1794520391\tgateway_configured\tworker=B artifact=9138e82e",
            "1794520491\tdispatch_result\trun_id=other-run status=done",
            "1794520491\tartifact_materialized\trun_id=run-1 files=4",
        )
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "manager-diag.log").write_text(log, encoding="utf-8")
            facts = self.driver.inspect_manager_diag(root, "run-1")
        self.assertEqual(facts.get("status"), "NOT_CONFIRMED", facts)
        self.assertEqual(facts.get("reason"), "manager_diag_event_missing:dispatch_result", facts)


if __name__ == "__main__":
    unittest.main()
