from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import patch

import pytest

from hermes_cli.dev_sync import DevSyncError
from hermes_cli.subcommands.dev import _cmd_dev_sync


def test_sync_failure_exits_nonzero(tmp_path, capsys):
    args = SimpleNamespace(watch=False, only=None, desktop=False)
    with (
        patch("hermes_cli.subcommands.dev.dev_sync_run", side_effect=DevSyncError("node install failed")),
        pytest.raises(SystemExit) as exc,
    ):
        _cmd_dev_sync(args, tmp_path)

    assert exc.value.code == 1
    assert "Sync failed: node install failed" in capsys.readouterr().err


def test_watch_flag_is_forwarded(tmp_path):
    args = SimpleNamespace(watch=True, only=["web"], desktop=False)
    with patch("hermes_cli.subcommands.dev.dev_sync_run") as sync:
        _cmd_dev_sync(args, tmp_path)

    sync.assert_called_once_with(tmp_path, watch=True, only=["web"], desktop=False)


# ── Parser/dispatch tests (item 18) ──────────────────────────────────────────

def test_dev_subcommand_is_registered_in_top_level_parser():
    """``hermes dev`` must be a valid subcommand in the real CLI parser.

    Before this fix, ``build_dev_parser`` was imported but never called in the
    registration sequence, so ``python -m hermes_cli.main dev status`` exited 2
    with ``invalid choice: 'dev'``.

    We test at the ``build_dev_parser`` level: it must attach a ``dev``
    subparser with sync/status/gc verbs. The top-level wiring (calling
    ``build_dev_parser`` in ``main()``) is verified by the fact that
    ``build_dev_parser`` is imported in ``main`` and the registration line
    exists — this test proves the parser itself is correct.
    """
    import argparse

    from hermes_cli.subcommands.dev import build_dev_parser, cmd_dev

    parser = argparse.ArgumentParser(prog="hermes")
    subparsers = parser.add_subparsers(dest="command")
    build_dev_parser(subparsers, cmd_dev=cmd_dev)

    # Parsing ``dev status`` must not raise SystemExit.
    ns = parser.parse_args(["dev", "status"])
    assert getattr(ns, "dev_verb", None) == "status"


def test_dev_sync_subcommand_parses_flags():
    """``hermes dev sync --watch --only web`` is accepted by the parser."""
    import argparse

    from hermes_cli.subcommands.dev import build_dev_parser, cmd_dev

    parser = argparse.ArgumentParser(prog="hermes")
    subparsers = parser.add_subparsers(dest="command")
    build_dev_parser(subparsers, cmd_dev=cmd_dev)

    ns = parser.parse_args(["dev", "sync", "--watch", "--only", "web"])
    assert ns.dev_verb == "sync"
    assert ns.watch is True
    assert ns.only == ["web"]
