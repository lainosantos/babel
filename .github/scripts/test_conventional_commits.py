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

    def new_branch(self, head, commits=(), name="feature"):
        # Model the full checkout after GitHub has created the remote ref.
        self.git("update-ref", f"refs/remotes/origin/{name}", head)
        return {"before": "0" * 40, "after": head, "ref": f"refs/heads/{name}",
                "commits": [{"id": sha} for sha in commits]}

    def test_subjects(self):
        for value in ["feat: add routing", "fix(audio): preserve channels", "perf!: isolate workers", "custom(ui)!: change settings", "docs: explain naïve resampling"]:
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

    def test_new_branch_excludes_legacy_ancestors_in_event(self):
        legacy = self.commit("Legacy history")
        self.git("update-ref", "refs/remotes/origin/main", legacy)
        first = self.commit("fix: first change")
        head = self.commit("docs: describe change")
        event = self.new_branch(head, [legacy, first, head])
        self.assertEqual(set(policy.incoming_commits("push", event, self.repo)), {first, head})
        self.assertEqual(policy.validate("push", event, self.repo), [])

    def test_new_branch_from_older_default_history_has_no_new_commits(self):
        legacy = self.commit("Legacy history")
        current = self.commit("Another legacy commit")
        self.git("update-ref", "refs/remotes/origin/main", current)
        for commits in [[], [legacy]]:
            with self.subTest(commits=commits):
                event = self.new_branch(legacy, commits)
                self.assertEqual(policy.incoming_commits("push", event, self.repo), [])
                self.assertEqual(policy.validate("push", event, self.repo), [])

    def test_new_branch_excludes_established_nondefault_branch_history(self):
        base = self.commit("Legacy main history")
        self.git("update-ref", "refs/remotes/origin/main", base)
        legacy_topic = self.commit("Legacy topic history")
        self.git("update-ref", "refs/remotes/origin/old-topic", legacy_topic)
        invalid = self.commit("New unformatted change")
        head = self.commit("fix: new topic change")
        event = self.new_branch(head, [base, legacy_topic, invalid, head])
        problems = policy.validate("push", event, self.repo)
        self.assertEqual(len(problems), 1)
        self.assertIn(invalid[:12], problems[0])

    def test_new_branch_checks_new_commits_omitted_from_payload(self):
        legacy = self.commit("Legacy history")
        self.git("update-ref", "refs/remotes/origin/main", legacy)
        invalid = self.commit("New unformatted change omitted by event limit")
        head = self.commit("fix: newest change")
        for commits in [[], [head]]:
            with self.subTest(commits=commits):
                problems = policy.validate("push", self.new_branch(head, commits), self.repo)
                self.assertEqual(len(problems), 1)
                self.assertIn(invalid[:12], problems[0])

    def test_initial_branch_checks_root_and_ignores_its_symbolic_alias(self):
        invalid = self.commit("Unformatted initial commit")
        head = self.commit("feat: initial application")
        event = self.new_branch(head, [head], name="main")
        self.git("symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main")
        self.assertEqual(set(policy.incoming_commits("push", event, self.repo)), {invalid, head})
        problems = policy.validate("push", event, self.repo)
        self.assertEqual(len(problems), 1)
        self.assertIn(invalid[:12], problems[0])

    def test_new_orphan_branch_checks_all_new_history(self):
        legacy = self.commit("Legacy history")
        self.git("update-ref", "refs/remotes/origin/main", legacy)
        self.git("checkout", "--quiet", "--orphan", "unrelated")
        invalid = self.commit("Unformatted orphan root")
        head = self.commit("feat: unrelated branch")
        problems = policy.validate("push", self.new_branch(head, [head]), self.repo)
        self.assertEqual(len(problems), 1)
        self.assertIn(invalid[:12], problems[0])

    def test_new_branch_rejects_invalid_ref_and_incomplete_history(self):
        head = self.commit("feat: initial commit")
        event = self.new_branch(head)
        for ref in ["--all", "refs/tags/v1", "refs/heads/feature..bad"]:
            with self.subTest(ref=ref):
                with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                    policy.incoming_commits("push", dict(event, ref=ref), self.repo)
        (self.repo / ".git/shallow").write_text(head + "\n", encoding="ascii")
        with self.assertRaisesRegex(ValueError, "complete Git history"):
            policy.incoming_commits("push", event, self.repo)

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
