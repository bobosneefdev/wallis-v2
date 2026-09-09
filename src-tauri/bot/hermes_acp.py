"""Run ACP with the effort selected for this Wallis worker."""
import hermes_bootstrap  # Hermes requires this before other imports.
import os

from acp_adapter.entry import main
from acp_adapter.server import HermesACPAgent, _extract_text
from acp.schema import PromptResponse
from hermes_constants import parse_reasoning_effort
from run_agent import AIAgent

effort = parse_reasoning_effort(os.environ.pop("WALLIS_REASONING_EFFORT", "medium"))
if effort is None:
    raise ValueError("Invalid Wallis reasoning effort")

# ACP currently omits reasoning_config on session creation AND model switches.
# Keep the fix local to this process; remove when ACP exposes working effort options.
original_init = AIAgent.__init__


def init_with_effort(self, *args, **kwargs):
    kwargs.setdefault("reasoning_config", effort.copy())
    original_init(self, *args, **kwargs)


AIAgent.__init__ = init_with_effort

# A late voice window may race the active turn's final token. Hermes normally rewrites an idle
# /steer into a new prompt; Wallis requires strict one-request-at-a-time steering instead.
original_prompt = HermesACPAgent.prompt


async def prompt_with_strict_steer(self, prompt, session_id, **kwargs):
    text = _extract_text(prompt).strip()
    if text == "/steer" or text.startswith("/steer "):
        guidance = text.removeprefix("/steer").strip()
        state = self.session_manager.get_session(session_id)
        if not guidance or state is None:
            return PromptResponse(stop_reason="refusal")
        with state.runtime_lock:
            if not state.is_running or not hasattr(state.agent, "steer") or not state.agent.steer(guidance):
                return PromptResponse(stop_reason="refusal")
        return PromptResponse(stop_reason="end_turn")
    return await original_prompt(self, prompt, session_id, **kwargs)


HermesACPAgent.prompt = prompt_with_strict_steer

if __name__ == "__main__":
    main()
