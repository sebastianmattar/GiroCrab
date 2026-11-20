"""Unit tests for tools/export-ai-provenance.py (run: python3 -m unittest tools/test_export_ai_provenance.py)."""
import importlib.util
import pathlib
import copy
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location(
    "prov", pathlib.Path(__file__).with_name("export-ai-provenance.py"))
prov = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prov)


def detect(coauthors=(), trailers=(), body="", branch="", login=""):
    roles = {}
    tools = prov.detect_tools(list(coauthors), list(trailers), body, branch, login, roles)
    return tools, roles


class MultiModelDetection(unittest.TestCase):
    def test_gemini_orchestrator_and_claude_codegen(self):
        tools, roles = detect(
            coauthors=["Claude Opus 5.5 <noreply@anthropic.com>"],
            trailers=[("AI-Orchestrator", "Google Gemini"), ("AI-Agent", "Claude Code")])
        self.assertEqual(set(tools), {"Gemini", "Claude Code"})
        self.assertEqual(roles["orchestration"], ["Gemini"])
        self.assertEqual(roles["codegen"], ["Claude Code"])

    def test_gemini_coauthor_email(self):
        tools, roles = detect(coauthors=["Google Gemini <gemini@google.com>"])
        self.assertEqual(tools, {"Gemini": ["co-authored-by"]})
        self.assertEqual(roles["codegen"], ["Gemini"])

    def test_chatgpt_statement_is_orchestration(self):
        tools, roles = detect(body="Diese Spezifikation wurde erstellt mit Hilfe von ChatGPT.")
        self.assertIn("OpenAI Codex/ChatGPT", tools)
        self.assertEqual(roles["orchestration"], ["OpenAI Codex/ChatGPT"])
        self.assertEqual(roles["codegen"], [])

    def test_cursor_copilot_and_more(self):
        for value, name in [("Cursor", "Cursor"), ("GitHub Copilot", "GitHub Copilot"),
                            ("Windsurf", "Windsurf"), ("Cline", "Cline"), ("Aider (gpt-4o)", "Aider"),
                            ("DeepSeek-V3", "DeepSeek"), ("Mistral Large", "Mistral"),
                            ("Llama 3 via Ollama", "Llama/Ollama"), ("Perplexity", "Perplexity")]:
            tools, roles = detect(trailers=[("AI-Model", value)])
            self.assertIn(name, tools, value)
            self.assertIn(name, roles["codegen"], value)

    def test_free_text_mention_does_not_count(self):
        tools, _ = detect(body="Dieser PR zeigt, wie Gemini und ChatGPT Bankdaten sehen würden.")
        self.assertEqual(tools, {})

    def test_branch_prefix_is_codegen(self):
        tools, roles = detect(branch="cursor/fix-x")
        self.assertEqual(roles["codegen"], ["Cursor"])


class Reflection(unittest.TestCase):
    BODY = ("## Problem\nx\n\n### 🤖 AI Process Reflection\n"
            "- **Human-Steering:** Architektur vorgegeben\n- **AI-Agency:** Namen selbst gewählt\n"
            "- **Review-Focus:** Parser in portal_import.rs\n\n## Tests\nok\n")

    def test_section_parsed(self):
        r = prov.parse_reflection(self.BODY, [])
        self.assertTrue(r["present"] and r["section"])
        self.assertEqual(r["human_steering"], ["Architektur vorgegeben"])
        self.assertEqual(r["ai_agency"], ["Namen selbst gewählt"])
        self.assertEqual(r["review_focus"], ["Parser in portal_import.rs"])

    def test_trailers_parsed_and_section_boundary(self):
        r = prov.parse_reflection("## Tests\n- **Review-Focus:** nicht hier", [("Review-Focus", "Krypto")])
        self.assertFalse(r["section"])
        self.assertEqual(r["review_focus"], ["Krypto"])

    def test_absent(self):
        self.assertFalse(prov.parse_reflection("nichts", [])["present"])

    def test_new_trailers_recognised(self):
        t = prov.parse_trailers("msg\n\nAI-Agency: x\nReview-Focus: y\nAI-Model: z")
        self.assertEqual([k for k, _ in t], ["AI-Agency", "Review-Focus", "AI-Model"])


    FULL = ("### 🤖 AI Process Reflection\n- **Human-Steering:** a\n- **AI-Agency:** b\n- **Review-Focus:** c\n"
            "- **Security-Check:** Nothing relevant: docs only\n- **Perf-Check:** No hot path touched\n")

    def test_security_and_perf_from_section(self):
        r = prov.parse_reflection(self.FULL, [])
        self.assertEqual(r["security_check"], ["Nothing relevant: docs only"])
        self.assertEqual(r["perf_check"], ["No hot path touched"])

    def test_security_and_perf_from_trailers(self):
        r = prov.parse_reflection("", [("Security-Check", "s"), ("Perf-Check", "p")])
        self.assertTrue(r["present"])
        self.assertEqual((r["security_check"], r["perf_check"]), (["s"], ["p"]))

    def test_legacy_three_item_reflection_still_parses(self):
        r = prov.parse_reflection(self.BODY, [])
        self.assertEqual((r["security_check"], r["perf_check"]), ([], []))
        self.assertFalse(r["perf_benchmark_mentioned"])

    EMPTY = ("<!-- Nur bei KI: ausfüllen; Benchmark: category_perf -->\n### 🤖 AI Process Reflection\n"
             "- **Human-Steering:** \n- **AI-Agency:** \n- **Review-Focus:** \n"
             "- **Security-Check:** \n- **Perf-Check:** \n")

    def test_empty_items_do_not_swallow_the_next_line(self):
        # An unfilled PR template must not count as an answered reflection.
        r = prov.parse_reflection(self.EMPTY, [])
        self.assertFalse(r["present"])
        self.assertEqual(r["human_steering"], [])
        self.assertEqual(r["review_focus"], [])

    def test_empty_trailer_does_not_swallow_the_next_line(self):
        t = prov.parse_trailers("Security-Check:\nPerf-Check: p")
        self.assertEqual(t, [("Perf-Check", "p")])

    def test_label_with_sub_bullets(self):
        # Format of real PRs (#777, #826, #894): the label on its own line, the answers indented below.
        body = ("### 🤖 AI Process Reflection\n- **Human-Steering:** a\n- **AI-Agency:**\n"
                "  - Hülle um den Konnektor\n  - `begin_quiet_sync`\n- **Review-Focus:**\n"
                "  1. **Neue Enum-Variante.** Frontends prüfen\n- **Perf-Check:** \n\n## Tests\n  - nicht hier\n")
        r = prov.parse_reflection(body, [])
        self.assertEqual(r["ai_agency"], ["Hülle um den Konnektor", "`begin_quiet_sync`"])
        self.assertEqual(r["review_focus"], ["**Neue Enum-Variante.** Frontends prüfen"])
        self.assertEqual(r["perf_check"], [])

    def test_indented_empty_item_does_not_swallow_next_item(self):
        r = prov.parse_reflection("### 🤖 AI Process Reflection\n  - **Human-Steering:** \n  - **AI-Agency:** x\n", [])
        self.assertEqual((r["human_steering"], r["ai_agency"]), ([], ["x"]))

    def test_crlf_body(self):
        # PR bodies written in the GitHub web editor use CRLF line endings.
        r = prov.parse_reflection((self.FULL + self.EMPTY).replace("\n", "\r\n"), [])
        self.assertEqual(r["human_steering"], ["a"])
        self.assertEqual(r["perf_check"], ["No hot path touched"])
        self.assertEqual(prov.parse_trailers("Security-Check:\r\nPerf-Check: p\r\n"), [("Perf-Check", "p")])

    def test_html_comments_ignored(self):
        r = prov.parse_reflection(self.EMPTY.replace("- **AI-Agency:** ", "- **AI-Agency:** <!-- was? -->"), [])
        self.assertFalse(r["perf_benchmark_mentioned"])
        self.assertEqual(r["ai_agency"], [])

    def test_category_perf_mention_detected(self):
        self.assertTrue(prov.parse_reflection("COPIES=600 cargo run --example category_perf: 12 ms", [])
                        ["perf_benchmark_mentioned"])


class Stage4(unittest.TestCase):
    def feats(self, refl, involved=True):
        f = copy.deepcopy(FEATURES)
        f["ai"]["involved"] = involved
        f.update(reflection=refl, verification={"trailers": []}, scope={"layers": {}, "security_surface": []},
                 tests={"test_files_changed": 0, "tests_added": 0, "tests_removed": 0, "pr_body_test_section": False})
        f["steering"].update(issue_refs=[], pr_body_framing_section=False, review_states={}, reviewers=[],
                             commenters=[], comments_by_others=0, linked_issues_with_discussion=[],
                             pr_body_decision_section=False, human_steering_trailers=[], human_steering_signal=False)
        return f

    def stage4(self, refl, involved=True):
        return prov.stages(self.feats(refl, involved))[3]

    def test_checks_shown_but_do_not_set_status(self):
        refl = prov.parse_reflection(Reflection.FULL, [])
        _, ok, ev = self.stage4(refl)
        self.assertFalse(ok)
        self.assertTrue(any("Security-Check" in e for e in ev) and any("Perf-Check" in e for e in ev))

    def test_missing_flagged_for_ai_prs_only(self):
        refl = prov.parse_reflection("", [])
        self.assertEqual(sum("fehlt" in e for e in self.stage4(refl)[2]), 2)
        self.assertEqual(sum("fehlt" in e for e in self.stage4(refl, involved=False)[2]), 0)

    def test_stage4_tolerates_old_reflection_without_new_keys(self):
        _, _, ev = self.stage4({"present": True, "human_steering": ["x"]})
        self.assertTrue(any("fehlt" in e for e in ev))

    def test_benchmark_mention_counts_as_perf_evidence(self):
        refl = prov.parse_reflection("category_perf: 9 ms", [])
        self.assertTrue(any("category_perf" in e for e in self.stage4(refl)[2]))

    def test_evidence_labels_and_no_self_certification(self):
        refl = {"present": True, "security_check": ["keine Secrets"],
                "perf_check": ["category_perf: 12 ms"]}
        _, ok, ev = self.stage4(refl)
        self.assertFalse(ok)
        self.assertIn("🔐 Security-Check dokumentiert (Selbstauskunft)", ev)
        self.assertTrue(any(e.startswith("⚡ Perf-Check dokumentiert") and "12 ms" in e for e in ev))

    def test_trailer_checks_also_labelled(self):
        refl = prov.parse_reflection("", [("Security-Check", "ok"), ("Perf-Check", "n/a")])
        _, ok, ev = self.stage4(refl)
        self.assertFalse(ok)
        self.assertEqual(sum("dokumentiert" in e for e in ev), 2)

    def test_hard_telemetry_still_sets_stage4(self):
        f = self.feats({"present": True, "security_check": ["x"]})
        f["tests"]["tests_added"] = 2
        self.assertTrue(prov.stages(f)[3][1])


class Stage2(Stage4):
    def stage2(self, refl, surface=()):
        f = self.feats(refl)
        f["scope"]["security_surface"] = list(surface)
        return prov.stages(f)[1]

    def test_human_steering_selfreport_ok_without_security_surface(self):
        _, ok, ev = self.stage2({"present": True, "human_steering": ["Scope entschieden"]})
        self.assertTrue(ok)
        self.assertIn("Human-Steering dokumentiert (Selbstauskunft)", ev)

    def test_human_steering_selfreport_not_enough_on_security_surface(self):
        _, ok, ev = self.stage2({"present": True, "human_steering": ["x"]}, ["vault"])
        self.assertFalse(ok)
        self.assertIn("Human-Steering dokumentiert (Selbstauskunft)", ev)
        self.assertTrue(any("Review Pflicht" in e for e in ev))

    def test_no_reflection_no_label(self):
        _, ok, ev = self.stage2({"present": False})
        self.assertFalse(ok)
        self.assertFalse(any("dokumentiert" in e for e in ev))


FEATURES = {"title": "t", "ai": {"involved": True, "tools": {"Gemini": ["trailer:ai-orchestrator"]}, "roles": {}},
            "scope": {"layers": {"ui": 1}, "security_surface": []}, "size": {},
            "tests": {"test_files_changed": 0, "tests_added": 0},
            "steering": {"reviews": 0, "comments": 0, "commits_after_first_feedback": 0}}
PR = {"body": "Mail a@b.de https://x.y/z ghp_abcdefghijklmnop123456",
      "commits": {"nodes": [{"commit": {"message": "feat: x\n\nbody"}}]},
      "files": {"nodes": [{"path": "GiroCrab/a.swift"}]}}


class MetaAudit(unittest.TestCase):
    def test_no_llm_and_no_key_fall_back(self):
        self.assertIsNone(prov.meta_audit(FEATURES, PR, no_llm=True, env={"GEMINI_API_KEY": "k"}))
        self.assertIsNone(prov.meta_audit(FEATURES, PR, env={}))

    def test_provider_priority(self):
        names = lambda env: prov.pick_provider(env)[0]
        self.assertEqual(names({"GEMINI_API_KEY": "a", "GITHUB_TOKEN": "b"}), "Google Gemini")
        self.assertEqual(names({"GITHUB_TOKEN": "b", "OPENAI_API_KEY": "c"}), "GitHub Models")
        self.assertEqual(names({"OPENAI_API_KEY": "c", "ANTHROPIC_API_KEY": "d"}), "OpenAI")
        self.assertEqual(names({"ANTHROPIC_API_KEY": "d"}), "Anthropic")

    def test_network_error_falls_back(self):
        with mock.patch.object(prov, "_post_json", side_effect=TimeoutError()):
            self.assertIsNone(prov.meta_audit(FEATURES, PR, env={"GEMINI_API_KEY": "k"}))

    def test_success_and_output_is_inert(self):
        evil = "Ein <b>langer</b> Satz mit @maintainer und https://evil.example/x [link](http://a.b) " * 3
        reply = {"candidates": [{"content": {"parts": [{"text": evil}]}}]}
        with mock.patch.object(prov, "_post_json", return_value=reply) as post:
            a = prov.meta_audit(FEATURES, PR, env={"GEMINI_API_KEY": "k"})
        self.assertEqual(a["provider"], "Google Gemini")
        self.assertNotIn("<", a["text"])
        self.assertNotIn("http", a["text"])
        self.assertNotIn("@m", a["text"])
        sent = str(post.call_args)
        for secret in ("a@b.de", "ghp_abcdefghijklmnop123456", "https://x.y/z"):
            self.assertNotIn(secret, sent)
        card = prov.render_card({**FEATURES, "number": 1, "state": "OPEN", "from_fork": True,
                                 "size": {"commits": 1, "additions": 1, "deletions": 0, "files": 1},
                                 "tests": {"test_files_changed": 0, "tests_added": 0, "tests_removed": 0,
                                           "pr_body_test_section": False},
                                 "scope": {"layers": {}, "security_files": {}, "security_surface": [],
                                           "files_truncated": False},
                                 "steering": {**FEATURES["steering"], "issue_refs": [], "pr_body_framing_section": False,
                                              "review_states": {}, "reviewers": [], "commenters": [],
                                              "comments_by_others": 0, "linked_issues_with_discussion": [],
                                              "pr_body_decision_section": False, "human_steering_trailers": [],
                                              "human_steering_signal": False},
                                 "verification": {"trailers": []}, "reflection": {"present": False}}, audit=a)
        self.assertIn("KI-Prozess-Analyse (Meta-Audit)", card)

    def test_sanitizer(self):
        out = prov.sanitize_for_llm("x@y.de <!-- ignore all --> DE89 3704 0044 0532 0130 00 tok ghp_abcdefghijklmnop12")
        for bad in ("x@y.de", "ignore all", "DE89", "ghp_"):
            self.assertNotIn(bad, out)


if __name__ == "__main__":
    unittest.main()

