# Ephemeral Relay with token sharing; history stays local

The Relay holds only in-memory live state and grants board access via a shared token/URL — no accounts, no database. Each Collector keeps its own durable session history on its machine, so history/cost analytics are a local-mode feature. Rejected: a persistent Relay with user accounts, which would enable fleet-wide history but turns the project into an operated SaaS (DB, auth, retention). A Relay restart loses nothing: Collectors re-push current state.
