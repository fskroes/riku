# Board acts on local sessions only; no remote control via the Relay

Interacting with a session from the board is limited to deep-linking into the local terminal/workspace. Responding to prompts in-board is a possible v2 for local sessions only; controlling a teammate's session through the Relay is explicitly out of scope forever. The Relay therefore carries a one-way, read-only stream — it never transports commands — which keeps the team story free of a control protocol, authorization model, and the blast radius of remotely approving another machine's agent actions.
