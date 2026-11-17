"""dashboard_watch: each project's watch state, the registry alias join and the row flags (#10896).

Covers: every state (Watched, Not watched, Watch pending, Watch failed) and its precedence (a new request wins, the
newest failed request shows unless an applied one is newer, a dismissed one never counts); an absent local/watch.json
is "watch list not created yet", never "Not watched", and stays absent; a malformed one is an error, never an empty
list; the alias join in tm's order (exact, ASCII case, managed checkout path) through registered_projects; the
path_exists and alias_mismatch flags; and ProjectsView shows a just-written request inside its 10 s answer cache.
Test: tests/test_dashboard_watch.py
"""
import json
import os
import tempfile
import unittest
from unittest import mock

import dashboard_projects as dp
import dashboard_watch as dw


def doc(doc_id, key, status, created, action="watch", version=1, **extra):
    data = {"key": key, "action": action, "status": status, "created": created, "by": "owner", **extra}
    return {"id": doc_id, "version": version, "data": data}


def write_watch(arch, keys):
    os.makedirs(os.path.join(arch, "local"), exist_ok=True)
    with open(os.path.join(arch, "local", "watch.json"), "w") as f:
        json.dump({"version": 1, "projects": [{"key": k} for k in keys]}, f)


class Tmp(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name


class State(Tmp):
    def test_list_membership_gives_watched_and_not_watched(self):
        write_watch(self.tmp, ["web"])
        keys = dw.read_watch(self.tmp)
        self.assertEqual(dw.state("web", keys, [])["label"], "Watched")
        self.assertEqual(dw.state("api", keys, [])["label"], "Not watched")

    def test_a_new_request_is_pending_over_the_list_and_over_a_failure(self):
        docs = [doc("A", "web", "failed", "2026-11-10T10:00:00Z", message="exit 1: boom"),
                doc("B", "web", "new", "2026-11-10T09:00:00Z", action="unwatch")]
        got = dw.state("web", {"web"}, docs)
        self.assertEqual((got["state"], got["label"]), ("pending", "Watch pending"))
        self.assertEqual((got["request"]["id"], got["request"]["action"]), ("B", "unwatch"))

    def test_the_newest_failed_request_shows_with_its_message_and_version(self):
        docs = [doc("A", "web", "failed", "2026-11-10T09:00:00Z", message="old"),
                doc("B", "web", "failed", "2026-11-10T10:00:00Z", version=4, message="exit 1: not registered")]
        got = dw.state("web", set(), docs)
        self.assertEqual((got["label"], got["request"]["id"], got["request"]["version"], got["request"]["message"]),
                         ("Watch failed", "B", 4, "exit 1: not registered"))

    def test_a_dismissed_failure_falls_back_to_the_list(self):
        docs = [doc("A", "web", "dismissed", "2026-11-10T10:00:00Z", message="exit 1")]
        self.assertEqual(dw.state("web", set(), docs)["label"], "Not watched")

    def test_a_newer_applied_request_supersedes_an_older_failure(self):
        # AC4: after the applier runs the row must match the list, so an old failure does not outlive a newer success
        docs = [doc("A", "web", "failed", "2026-11-10T09:00:00Z"), doc("B", "web", "applied", "2026-11-10T10:00:00Z")]
        self.assertEqual(dw.state("web", {"web"}, docs)["label"], "Watched")
        docs.append(doc("C", "web", "failed", "2026-11-10T11:00:00Z", action="unwatch"))
        self.assertEqual(dw.state("web", {"web"}, docs)["request"]["id"], "C")

    def test_another_keys_requests_do_not_count(self):
        docs = [doc("A", "api", "new", "2026-11-10T09:00:00Z")]
        self.assertEqual(dw.state("web", {"web"}, docs)["label"], "Watched")


class WatchFile(Tmp):
    def test_absent_list_is_not_created_and_reads_as_unknown_never_not_watched(self):
        path = os.path.join(self.tmp, "local", "watch.json")
        self.assertIsNone(dw.read_watch(self.tmp))
        out = dw.overlay({"projects": [{"id": "web"}]}, self.tmp, [])
        self.assertEqual(out["projects"][0]["watch"]["label"], "watch list not created yet")
        self.assertNotEqual(out["projects"][0]["watch"]["label"], "Not watched")
        self.assertEqual(out["watch_list"], {"present": False, "error": None})
        self.assertFalse(os.path.exists(path))
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "local")))

    def test_pending_and_failed_still_show_while_the_list_is_absent(self):
        docs = [doc("A", "web", "new", "2026-11-10T09:00:00Z"), doc("B", "api", "failed", "2026-11-10T09:00:00Z")]
        out = {p["id"]: p["watch"]["label"] for p in dw.overlay({"projects": [{"id": "web"}, {"id": "api"}]}, self.tmp, docs)["projects"]}
        self.assertEqual(out, {"web": "Watch pending", "api": "Watch failed"})

    def test_a_malformed_list_is_an_error_never_an_empty_list(self):
        os.makedirs(os.path.join(self.tmp, "local"))
        for body in ("{not json", json.dumps({"version": 2, "projects": []}), json.dumps({"projects": []})):
            with open(os.path.join(self.tmp, "local", "watch.json"), "w") as f:
                f.write(body)
            with self.assertRaises(dw.WatchListError):
                dw.read_watch(self.tmp)
            out = dw.overlay({"projects": [{"id": "web"}]}, self.tmp, [])
            self.assertEqual(out["projects"][0]["watch"]["label"], "watch list unreadable")
            self.assertTrue(out["watch_list"]["present"])
            self.assertIn("watch.json", out["watch_list"]["error"])


class AliasJoin(Tmp):
    def test_exact_then_ascii_case_then_checkout_path(self):
        checkout = os.path.join(self.tmp, "acme", "web")
        rows = [{"alias": "WEB", "path": "/x/case"}, {"alias": "web", "path": "/x/exact"}, {"alias": "site", "path": checkout}]
        self.assertEqual(dw.alias_for("web", checkout, rows)["alias"], "web")
        self.assertEqual(dw.alias_for("Web", checkout, rows[2:] + rows[:1])["alias"], "WEB")
        self.assertEqual(dw.alias_for("web", checkout + "/", rows[2:])["alias"], "site")   # Rust same_path trims a trailing "/"
        self.assertIsNone(dw.alias_for("web", None, rows[2:]))
        self.assertIsNone(dw.alias_for("İweb", None, [{"alias": "i̇web", "path": "/x"}]))   # ASCII case only, as Rust

    def test_registered_projects_finds_the_alias_by_the_managed_checkout_path(self):
        pr = os.path.join(self.tmp, "projects")
        clone = os.path.join(pr, "acme", "widget-shop")
        os.makedirs(clone)
        root = os.path.join(self.tmp, "mpm")
        os.makedirs(os.path.join(root, "project-registry"))
        with open(os.path.join(root, "project-registry", "projects.json"), "w") as f:
            json.dump({"projects": {"widget-shop": {"name": "widget-shop", "repo_url": "https://github.com/ACME/Widget_Shop.git"}}}, f)
        with open(os.path.join(root, "project-paths.json"), "w") as f:
            json.dump([{"alias": "shop", "path": clone}], f)
        with mock.patch.dict(os.environ, {"ARCHITECT_PROJECTS_ROOT": pr, "ARCHITECT_CONFIG": ""}):
            reg = dp.registered_projects(root)
            out = dp.build(reg, {}, {}, {})["projects"][0]
        self.assertEqual((reg[0]["alias"], reg[0]["path"]), ("shop", clone))
        self.assertEqual((out["session_alias"], out["alias_mismatch"], out["path_exists"]), ("shop", True, True))

    def test_flags_match_tm_fleet_project_list(self):
        self.assertEqual(dw.flags("web", self.tmp, "web"), {"path_exists": True, "alias_mismatch": False})
        self.assertEqual(dw.flags("web", os.path.join(self.tmp, "gone"), "Web"), {"path_exists": False, "alias_mismatch": True})
        self.assertEqual(dw.flags("web", None, None), {"path_exists": False, "alias_mismatch": False})


class Freshness(Tmp):
    def test_a_request_written_inside_the_answer_cache_shows_at_once(self):
        write_watch(self.tmp, [])
        reg = [{"name": "web", "path": self.tmp, "owner": "acme", "repo": "web"}]
        view = dp.ProjectsView(lambda: reg, lambda: {}, panes=lambda: {}, clock=lambda: 1000.0, lifecycle=lambda: {},
                               arch_dir=self.tmp)
        self.assertEqual(view.get({}, [])["projects"][0]["watch"]["label"], "Not watched")
        docs = [doc("A", "web", "new", "2026-11-10T09:00:00Z")]
        self.assertEqual(view.get({}, docs)["projects"][0]["watch"]["label"], "Watch pending")   # same clock: cached answer
        write_watch(self.tmp, ["web"])
        self.assertEqual(view.get({}, [])["projects"][0]["watch"]["label"], "Watched")


if __name__ == "__main__":
    unittest.main()
