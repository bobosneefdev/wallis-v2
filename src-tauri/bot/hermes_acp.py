"""Run ACP with the effort selected for this Wallis worker."""
import hermes_bootstrap  # Hermes requires this before other imports.
import os

from acp_adapter.entry import main
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

if __name__ == "__main__":
    main()
