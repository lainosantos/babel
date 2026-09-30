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
        # A new branch must not revalidate years of pre-policy history. The
        # event lists incoming commits; a new ref with no new objects checks its tip.
        return list(dict.fromkeys(commit_sha(item["id"]) for item in payload.get("commits", []))) or [head]
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
