# Crossover benchmark source

Recovered from sprefa commit `e2052d5ae`, directory `v6/labs/exec_shootout/postgres_pglite_ivm`. The fixture generator and DD consumer are unchanged. The SQLite installer removes the retired metrics/version API calls and reports the extension hash. Reopen loads the extension because the current public result is a virtual table. The transport retains the original WAL/FULL transaction clock, snapshot materialization, exact input/output hashes, and untimed oracle.

`28_run.py` runs the recovered fixtures against these two consumers in separate processes. Three repetitions, no warmups, alternating arm order. Historical target: 12000 rows, batch 1000, fanout 200, counted SQLite 21.121 ms / volatile DD 1.118 ms.
