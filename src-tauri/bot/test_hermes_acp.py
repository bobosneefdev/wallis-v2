"""Run with the installed Hermes venv; no model requests or Discord messages."""
import hermes_bootstrap
import os
from pathlib import Path
import runpy
import unittest
from unittest.mock import patch

from run_agent import AIAgent
from acp_adapter.session import SessionManager


class EffortTest(unittest.TestCase):
    def test_worker_effort_reaches_agents_created_after_model_changes(self):
        for effort, expected in [("none", {"enabled": False}), ("high", {"enabled": True, "effort": "high"})]:
            with self.subTest(effort=effort), patch.dict(os.environ, {"WALLIS_REASONING_EFFORT": effort}):
                received = []
                with (patch.object(AIAgent, "__init__", lambda self, **kwargs: received.append(kwargs)),
                      patch("hermes_cli.config.load_config", return_value={"agent": {"reasoning_effort": "low"}}),
                      patch("hermes_cli.runtime_provider.resolve_runtime_provider", return_value={"provider": "openrouter"}),
                      patch("hermes_cli.mcp_startup.ensure_mcp_discovery_before_agent_build"),
                      patch.object(SessionManager, "_get_db", return_value=None)):
                    runpy.run_path(str(Path(__file__).with_name("hermes_acp.py")))
                    manager = SessionManager()
                    manager._make_agent(session_id="effort-check", cwd="/tmp", model="first-model")
                    manager._make_agent(session_id="effort-check", cwd="/tmp", model="second-model")
                self.assertEqual([item["reasoning_config"] for item in received], [expected, expected])


if __name__ == "__main__":
    unittest.main()
