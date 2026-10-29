# Search suite: report

`python3 bench/search_report.py bench/results/task-20261028-22*.json bench/results/task-20261028-23*.json`
(the 15 files from 221453 to 234020; 5 two-round batches per model). Arms: `grep`
(the tool), `search` (same tool, named `search`), `none` (no search tool).
Decision and caveats: docs/adr/0024-search-tool-stays-grep.md.

## anthropic/claude-haiku-5.5  (grep: 110 runs, search: 110 runs, none: 110 runs)

| arm | success | adoption (calls) | used the tool (runs) | requests/run | prompt tok/run | wall s/run | $/run | tool failures |
|---|---|---|---|---|---|---|---|---|
| grep | 107/110 | 0.57 | 0.72 | 4.23 | 18663 | 9.2 | 0.0015 | 0.2% |
| search | 108/110 | 0.62 | 0.73 | 4.34 | 18892 | 9.4 | 0.0015 | 1.2% |
| none | 108/110 | – | – | 4.19 | 16877 | 9.0 | 0.0014 | 2.2% |

Differences (95% bootstrap interval):

| question | difference | interval | |
|---|---|---|---|
| name: grep vs search: adoption (calls) | -0.048 | [-0.167, +0.0711] | no clear difference |
| name: grep vs search: used the tool (runs) | -0.0025 | [-0.121, +0.117] | no clear difference |
| tool vs no tool: grep vs none: requests/run | +0.0364 | [-0.309, +0.4] | no clear difference |
| tool vs no tool: grep vs none: prompt tok/run | +1.79e+03 | [-3.43e+03, +7.43e+03] | no clear difference |
| tool vs no tool: grep vs none: $/run | +3.85e-05 | [-0.00022, +0.000305] | no clear difference |
| tool vs no tool: grep vs none: wall s/run | +0.203 | [-1.17, +1.66] | no clear difference |
| tool vs no tool: search vs none: requests/run | +0.145 | [-0.2, +0.5] | no clear difference |
| tool vs no tool: search vs none: prompt tok/run | +2.01e+03 | [-3.22e+03, +7.41e+03] | no clear difference |
| tool vs no tool: search vs none: $/run | +9.63e-05 | [-0.000162, +0.000361] | no clear difference |
| tool vs no tool: search vs none: wall s/run | +0.445 | [-0.918, +1.85] | no clear difference |
| name: grep vs search: requests/run | -0.109 | [-0.464, +0.236] | no clear difference |
| name: grep vs search: prompt tok/run | -229 | [-5.84e+03, +5.66e+03] | no clear difference |
| name: grep vs search: $/run | -5.78e-05 | [-0.000329, +0.000208] | no clear difference |
| name: grep vs search: tool failures | -0.00941 | [-0.0196, +2.65e-05] | no clear difference |
| success: grep vs none | -0.9% | Fisher p=1.00 | |
| success: search vs none | +0.0% | Fisher p=1.00 | |
| success: grep vs search | -0.9% | Fisher p=1.00 | |

Adoption by task (calls; `grep` arm / `search` arm):

| task | callers | config | count | definition | hard-env | hard-errorcode | hard-rename | hard-trace | hard-unused | rename | signature |
|---|---|---|---|---|---|---|---|---|---|---|---|
| grep / search | 0.48 / 0.48 | 0.00 / 0.27 | 0.00 / 0.00 | 0.91 / 1.00 | 0.94 / 0.94 | 1.00 / 1.00 | 0.45 / 0.42 | 0.76 / 0.92 | 0.08 / 0.00 | 0.54 / 0.61 | 0.44 / 0.43 |

## deepseek/deepseek-v4.1-flash-1017  (grep: 110 runs, search: 110 runs, none: 110 runs)

| arm | success | adoption (calls) | used the tool (runs) | requests/run | prompt tok/run | wall s/run | $/run | tool failures |
|---|---|---|---|---|---|---|---|---|
| grep | 110/110 | 0.37 | 0.80 | 6.53 | 43386 | 9.8 | 0.0019 | 1.7% |
| search | 110/110 | 0.25 | 0.69 | 6.71 | 45600 | 9.5 | 0.0029 | 2.5% |
| none | 107/110 | – | – | 7.04 | 44774 | 10.1 | 0.0024 | 2.6% |

Differences (95% bootstrap interval):

| question | difference | interval | |
|---|---|---|---|
| name: grep vs search: adoption (calls) | +0.12 | [+0.042, +0.197] | differs |
| name: grep vs search: used the tool (runs) | +0.109 | [-0.0023, +0.225] | no clear difference |
| tool vs no tool: grep vs none: requests/run | -0.509 | [-1.22, +0.191] | no clear difference |
| tool vs no tool: grep vs none: prompt tok/run | -1.39e+03 | [-1.67e+04, +1.37e+04] | no clear difference |
| tool vs no tool: grep vs none: $/run | -0.000515 | [-0.00127, +0.000276] | no clear difference |
| tool vs no tool: grep vs none: wall s/run | -0.243 | [-2.35, +1.71] | no clear difference |
| tool vs no tool: search vs none: requests/run | -0.327 | [-1.06, +0.427] | no clear difference |
| tool vs no tool: search vs none: prompt tok/run | +826 | [-1.61e+04, +1.71e+04] | no clear difference |
| tool vs no tool: search vs none: $/run | +0.000479 | [-0.000663, +0.00173] | no clear difference |
| tool vs no tool: search vs none: wall s/run | -0.57 | [-2.56, +1.35] | no clear difference |
| name: grep vs search: requests/run | -0.182 | [-0.873, +0.509] | no clear difference |
| name: grep vs search: prompt tok/run | -2.21e+03 | [-1.81e+04, +1.44e+04] | no clear difference |
| name: grep vs search: $/run | -0.000994 | [-0.00222, +0.00011] | no clear difference |
| name: grep vs search: tool failures | -0.00825 | [-0.021, +0.00412] | no clear difference |
| success: grep vs none | +2.7% | Fisher p=0.25 | |
| success: search vs none | +2.7% | Fisher p=0.25 | |
| success: grep vs search | +0.0% | Fisher p=1.00 | |

Adoption by task (calls; `grep` arm / `search` arm):

| task | callers | config | count | definition | hard-env | hard-errorcode | hard-rename | hard-trace | hard-unused | rename | signature |
|---|---|---|---|---|---|---|---|---|---|---|---|
| grep / search | 0.64 / 0.35 | 0.92 / 0.86 | 0.12 / 0.12 | 0.62 / 0.52 | 0.23 / 0.09 | 0.86 / 0.60 | 0.30 / 0.19 | 0.35 / 0.23 | 0.15 / 0.00 | 0.29 / 0.25 | 0.50 / 0.42 |

## openai/gpt-6-luna  (grep: 110 runs, search: 110 runs, none: 110 runs)

| arm | success | adoption (calls) | used the tool (runs) | requests/run | prompt tok/run | wall s/run | $/run | tool failures |
|---|---|---|---|---|---|---|---|---|
| grep | 108/110 | 0.84 | 0.87 | 5.38 | 14470 | 15.2 | 0.0010 | 2.2% |
| search | 106/110 | 0.82 | 0.83 | 5.25 | 15757 | 15.9 | 0.0011 | 2.6% |
| none | 106/110 | – | – | 5.39 | 18136 | 14.9 | 0.0012 | 2.0% |

Differences (95% bootstrap interval):

| question | difference | interval | |
|---|---|---|---|
| name: grep vs search: adoption (calls) | +0.0256 | [-0.0652, +0.118] | no clear difference |
| name: grep vs search: used the tool (runs) | +0.0434 | [-0.0564, +0.142] | no clear difference |
| tool vs no tool: grep vs none: requests/run | -0.00909 | [-0.382, +0.355] | no clear difference |
| tool vs no tool: grep vs none: prompt tok/run | -3.67e+03 | [-7e+03, -503] | differs |
| tool vs no tool: grep vs none: $/run | -0.000157 | [-0.000316, -9.01e-06] | differs |
| tool vs no tool: grep vs none: wall s/run | +0.347 | [-1.68, +2.55] | no clear difference |
| tool vs no tool: search vs none: requests/run | -0.136 | [-0.473, +0.182] | no clear difference |
| tool vs no tool: search vs none: prompt tok/run | -2.38e+03 | [-5.69e+03, +1.02e+03] | no clear difference |
| tool vs no tool: search vs none: $/run | -9.92e-05 | [-0.000253, +5.86e-05] | no clear difference |
| tool vs no tool: search vs none: wall s/run | +1.05 | [-0.954, +3.19] | no clear difference |
| name: grep vs search: requests/run | +0.127 | [-0.191, +0.473] | no clear difference |
| name: grep vs search: prompt tok/run | -1.29e+03 | [-4.04e+03, +1.5e+03] | no clear difference |
| name: grep vs search: $/run | -5.78e-05 | [-0.000195, +7.9e-05] | no clear difference |
| name: grep vs search: tool failures | -0.00478 | [-0.025, +0.0171] | no clear difference |
| success: grep vs none | +1.8% | Fisher p=0.68 | |
| success: search vs none | +0.0% | Fisher p=1.00 | |
| success: grep vs search | +1.8% | Fisher p=0.68 | |

Adoption by task (calls; `grep` arm / `search` arm):

| task | callers | config | count | definition | hard-env | hard-errorcode | hard-rename | hard-trace | hard-unused | rename | signature |
|---|---|---|---|---|---|---|---|---|---|---|---|
| grep / search | 0.71 / 0.54 | 1.00 / 0.89 | 0.00 / 0.00 | 1.00 / 1.00 | 1.00 / 0.81 | 1.00 / 1.00 | 0.81 / 0.69 | 1.00 / 1.00 | 0.88 / 0.93 | 0.86 / 0.90 | 0.71 / 0.38 |

merged openai/gpt-6-luna: /home/brandon/git/construct/.claude/worktrees/search-tool-bench-naming-ffc6ef/bench/results/search-merged-openai_gpt-6-luna.json

merged deepseek/deepseek-v4.1-flash-1017: /home/brandon/git/construct/.claude/worktrees/search-tool-bench-naming-ffc6ef/bench/results/search-merged-deepseek_deepseek-v4.1-flash-1017.json

merged anthropic/claude-haiku-5.5: /home/brandon/git/construct/.claude/worktrees/search-tool-bench-naming-ffc6ef/bench/results/search-merged-anthropic_claude-haiku-5.5.json
