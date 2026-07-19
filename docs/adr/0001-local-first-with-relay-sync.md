# Local-first board with relay sync for remote machines

Agent Board must serve solo use with zero setup and team/multi-machine use. We chose a local-first architecture: the board reads local Agent Session files directly, while remote machines run a Collector that pushes updates to a lightweight Relay the board also subscribes to. Rejected: a mandatory central server (kills zero-setup solo use) and direct SSH pull from remote machines (every viewer would need network access to every machine). Cost accepted: two ingestion paths, mitigated by sharing Collector code between them.
