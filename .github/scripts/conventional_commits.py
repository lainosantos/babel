#!/usr/bin/env python3
"""Validate only incoming commits and PR titles; leave existing history intact."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess


SUBJECT = re.compile(r"[a-z][a-z0-9-]*(?:\([^\r\n()]+\))?!?: \S[^\r\n]*\Z")
SHA = re.compile(r"[0-9a-f]{40}\Z")


def valid_subject(value: str) -> bool:
    # Conventional Commits allows types besides feat/fix and an optional scope.
    return SUBJECT.fullmatch(value) is not None


def commit_sha(value: str) -> str:
    if not SHA.fullmatch(value) or value == "0" * 40:
        raise ValueError("Expected a nonzero full Git commit SHA")
    return value


def git(*arguments: str, cwd: Path) -> str:
    return subprocess.run(
        ["git", *arguments], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.strip()


def new_branch_commits(payload: dict, head: str, cwd: Path) -> list[str]:
    # GitHub's new-ref comparison may contain the entire legacy ancestry, and
    # its commits array is capped at 2,048 entries. Walk the fetched graph instead.
    # The workflow uses checkout fetch-depth: 0, including all origin branches.
    ref = payload["ref"]
    if not isinstance(ref, str) or not ref.startswith("refs/heads/"):
        raise ValueError("Expected a full branch ref for a new-branch push")
    git("check-ref-format", ref, cwd=cwd)
    if git("rev-parse", "--is-shallow-repository", cwd=cwd) != "false":
        raise ValueError("New-branch validation requires complete Git history")
    pushed_remote = "refs/remotes/origin/" + ref.removeprefix("refs/heads/")
    established = []
    for line in git("for-each-ref", "--format=%(objectname) %(refname) %(symref)",
                    "refs/remotes/origin/", cwd=cwd).splitlines():
        fields = line.split()
        # Excluding the just-created ref or its symbolic origin/HEAD alias
        # would suppress every incoming commit, including invalid new ones.
        if len(fields) == 2 and fields[1] != pushed_remote:
            established.append(commit_sha(fields[0]))
    return git("rev-list", "--no-merges", head, "--not", *established, cwd=cwd).splitlines()


def incoming_commits(event: str, payload: dict, cwd: Path) -> list[str]:
    if event == "pull_request":
        pull = payload["pull_request"]
        base, head = commit_sha(pull["base"]["sha"]), commit_sha(pull["head"]["sha"])
        return git("rev-list", "--no-merges", f"{base}..{head}", cwd=cwd).splitlines()
    if event == "push":
        if payload.get("deleted"):
            return []
        head = commit_sha(payload["after"])
        before = payload.get("before", "0" * 40)
        if before != "0" * 40:
            return git("rev-list", "--no-merges", f"{commit_sha(before)}..{head}", cwd=cwd).splitlines()
        return new_branch_commits(payload, head, cwd)
    return [git("rev-parse", "HEAD", cwd=cwd)]


def validate(event: str, payload: dict, cwd: Path) -> list[str]:
    problems = []
    if event == "pull_request" and not valid_subject(payload["pull_request"]["title"]):
        problems.append("Pull request title must use: type(optional scope): description")
    for sha in incoming_commits(event, payload, cwd):
        # GitHub's generated merge commits do not follow this convention.
        if len(git("show", "-s", "--format=%P", sha, cwd=cwd).split()) > 1:
            continue
        subject = git("show", "-s", "--format=%s", sha, cwd=cwd)
        if not valid_subject(subject):
            problems.append(f"Commit {sha[:12]} must use: type(optional scope): description")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", default=os.environ.get("GITHUB_EVENT_NAME", "workflow_dispatch"))
    parser.add_argument("--event-path", type=Path, default=os.environ.get("GITHUB_EVENT_PATH"))
    parser.add_argument("--repository", type=Path, default=Path.cwd())
    args = parser.parse_args()
    payload = json.loads(args.event_path.read_text(encoding="utf-8")) if args.event_path else {}
    try:
        problems = validate(args.event, payload, args.repository)
    except (ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Cannot determine incoming commits: {type(error).__name__}")
        return 1
    for problem in problems:
        print(problem)
    if not problems:
        print("Conventional Commits validation passed.")
    return int(bool(problems))


if __name__ == "__main__":
    raise SystemExit(main())
