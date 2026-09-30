from pathlib import Path
import subprocess
import tempfile
import unittest

import conventional_commits as policy


class ConventionalCommitsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        self.git("init", "--quiet")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True, capture_output=True, text=True).stdout.strip()

    def commit(self, title):
        self.git("commit", "--quiet", "--allow-empty", "-m", title)
        return self.git("rev-parse", "HEAD")

    def test_subjects(self):
        for value in ["feat: add routing", "fix(audio): preserve channels", "perf!: isolate workers", "custom(ui)!: change settings", "docs: documentação"]:
            with self.subTest(value=value):
                self.assertTrue(policy.valid_subject(value))
        for value in ["Fix routing", "feat:", "feat: ", "feat:  padded", "feat: a\nfix: b", "feat(): empty scope"]:
            with self.subTest(value=value):
                self.assertFalse(policy.valid_subject(value))

    def test_push_preserves_legacy_history(self):
        legacy = self.commit("Legacy history")
        head = self.commit("fix: preserve audio")
        self.assertEqual(policy.validate("push", {"before": legacy, "after": head}, self.repo), [])

    def test_push_checks_all_new_commits(self):
        base = self.commit("Legacy history")
        invalid = self.commit("Unformatted change")
        head = self.commit("test: exercise routing")
        problems = policy.validate("push", {"before": base, "after": head}, self.repo)
        self.assertEqual(len(problems), 1)
        self.assertIn(invalid[:12], problems[0])

    def test_pr_validates_title_and_commit_range(self):
        base = self.commit("Legacy history")
        head = self.commit("feat: new behavior")
        pull = {"base": {"sha": base}, "head": {"sha": head}, "title": "Invalid title"}
        self.assertEqual(len(policy.validate("pull_request", {"pull_request": pull}, self.repo)), 1)
        pull["title"] = "feat: new behavior"
        self.assertEqual(policy.validate("pull_request", {"pull_request": pull}, self.repo), [])

    def test_new_branch_uses_incoming_event_not_legacy_ancestors(self):
        self.commit("Legacy history")
        first = self.commit("fix: first change")
        head = self.commit("docs: describe change")
        event = {"before": "0" * 40, "after": head, "commits": [{"id": first}, {"id": head}]}
        self.assertEqual(policy.incoming_commits("push", event, self.repo), [first, head])
        self.assertEqual(policy.validate("push", event, self.repo), [])

    def test_deleted_ref_and_invalid_sha(self):
        self.assertEqual(policy.incoming_commits("push", {"deleted": True}, self.repo), [])
        with self.assertRaises(ValueError):
            policy.incoming_commits("push", {"after": "--all"}, self.repo)

    def test_generated_merge_subject_is_exempt(self):
        base = self.commit("Legacy history")
        branch = self.git("branch", "--show-current")
        self.git("checkout", "--quiet", "-b", "feature")
        self.commit("fix: branch change")
        self.git("checkout", "--quiet", branch)
        self.commit("docs: branch notes")
        self.git("merge", "--no-ff", "--quiet", "feature", "-m", "Merge branch feature")
        head = self.git("rev-parse", "HEAD")
        self.assertEqual(policy.validate("push", {"before": base, "after": head}, self.repo), [])


if __name__ == "__main__":
    unittest.main()
