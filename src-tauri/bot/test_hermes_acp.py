"""Run with the installed Hermes venv; no model requests or Discord messages."""
import hermes_bootstrap
import asyncio
import os
from pathlib import Path
import runpy
from types import SimpleNamespace
import threading
import unittest
from unittest.mock import patch

from run_agent import AIAgent
from acp.schema import TextContentBlock
from acp_adapter.session import SessionManager


class EffortTest(unittest.TestCase):
    def test_idle_steer_cannot_start_an_llm_turn(self):
        namespace = runpy.run_path(str(Path(__file__).with_name("hermes_acp.py")))
        agent = SimpleNamespace(session_manager=SimpleNamespace(get_session=lambda _session_id: None))
        response = asyncio.run(namespace["prompt_with_strict_steer"](
            agent, [TextContentBlock(type="text", text="/steer newer speech")], "voice"
        ))
        self.assertEqual(response.stop_reason, "refusal")

        steers = []
        state = SimpleNamespace(is_running=True, runtime_lock=threading.Lock(),
                                agent=SimpleNamespace(steer=lambda text: steers.append(text) or True))
        agent.session_manager.get_session = lambda _session_id: state
        response = asyncio.run(namespace["prompt_with_strict_steer"](
            agent, [TextContentBlock(type="text", text="/steer newer speech")], "voice"
        ))
        self.assertEqual(response.stop_reason, "end_turn")
        self.assertEqual(steers, ["newer speech"])

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
