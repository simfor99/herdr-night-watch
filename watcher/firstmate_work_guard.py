#!/usr/bin/env python3
"""Read Firstmate's native pending-work records without changing its state.

Source contract: Firstmate bin/fm-supervision-lib.sh and docs/turnend-guard.md.
Task metadata remains in flight until Firstmate tears the task down. Unread
wake records require a continuation. Registered observers, plan observation
and custom polling checks alone do not prevent completion.
"""

import argparse
import json
from pathlib import Path


def pending_work(home: Path) -> dict:
    state = home / "state"
    try:
        if not (home / "bin" / "fm-supervision-lib.sh").is_file() or not state.is_dir():
            raise OSError("Firstmate home or state directory is unavailable")
        # iterdir surfaces unreadable state instead of silently treating it as empty.
        tasks = sum(path.name.endswith(".meta") for path in state.iterdir())
        sources_dir = state / "procevent"
        sources = sum(path.name.endswith(".source") for path in sources_dir.iterdir()) if sources_dir.exists() else 0
        queue = state / ".wake-queue"
        queued = queue.stat().st_size > 0 if queue.exists() else False
        return {
            "schema_version": 1,
            "status": "active" if tasks or queued else "ready",
            "detail": f"tasks={tasks}, registered_observers={sources}, queued_wake={queued}",
        }
    except OSError as error:
        return {"schema_version": 1, "status": "unknown", "detail": str(error)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", type=Path, required=True)
    print(json.dumps(pending_work(parser.parse_args().home)))
