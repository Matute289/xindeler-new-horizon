---
name: xindeler-telemetry
description: Use when the user asks you to analyze what happened in a game session, debug in-game behavior, review game state after a test, or investigate a bug using telemetry logs. Teaches you how to locate, parse, and interpret the telemetry logs produced by the logging-verbose build.
---

# Xindeler Telemetry Analysis

## When to invoke this skill

- User says "mirá los logs", "qué pasó en el juego", "revisá el telemetry", "analizá la sesión"
- You need to understand game state after a change was made and tested
- You're debugging a bug and need context beyond a screenshot
- You want to verify that a feature worked correctly in-game

## Log File Locations

```
userdata/voxygen/logs/          ← client logs (relative to project root)
  *_client_telemetry.jsonl      ← structured game state (this skill)
  *_client_info.log             ← human-readable operational log
  *_client_err.log              ← errors and warnings (always present)

userdata/server/logs/           ← server logs
  *_server_telemetry.jsonl
  *_server_info.log
  *_server_err.log
```

Files are named `YYYY-MM-DD_HHh_*` (hourly rotation). Always use the most recent file unless the user specifies otherwise.

## Step 1 — Locate the latest telemetry file

```bash
ls -lt userdata/voxygen/logs/*telemetry* 2>/dev/null | head -5
```

If no telemetry file exists, the game was run without `--features logging-verbose`. Tell the user and suggest:
```bash
cargo build --bin xindeler-voxygen --features xindeler-voxygen/logging-verbose
```

## Step 2 — Session Overview

Always start with a session summary before drilling down:

```bash
# Session boundaries
grep -h '"t":"ss"\|"t":"se"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null | tail -10

# Event type distribution (what happened most)
grep -oh '"t":"[^"]*"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null \
  | sort | uniq -c | sort -rn | head -20

# Time span of the session
grep -h '"t":"ss"\|"t":"se"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null \
  | python3 -c "import sys,json; lines=[json.loads(l) for l in sys.stdin]; print('Start:',lines[0].get('ts') if lines else 'N/A'); print('End:',lines[-1].get('ts') if len(lines)>1 else 'still running')"
```

## Step 3 — Player State Timeline

Get a feel for how the player was doing over the session:

```bash
# Health over time (snapshots)
grep '"t":"ps"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null \
  | python3 -c "
import sys, json
for line in sys.stdin:
    e = json.loads(line)
    print(f\"{e['ts'][11:19]} hp={e.get('hp')}/{e.get('hp_max')} st={e.get('st')}/{e.get('st_max')} state={e.get('state')} pos={e.get('pos')}\")
" | tail -30

# All player deaths
grep '"t":"pd"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null | python3 -c "
import sys, json
for line in sys.stdin:
    e = json.loads(line)
    print(f\"DEATH at {e['ts'][11:19]}: cause={e.get('cause')} killer={e.get('killer')} survived={e.get('survived_s')}s pos={e.get('pos')}\")
"
```

## Step 4 — Combat Analysis

```bash
# All combat hits (who hit whom, how much damage)
grep '"t":"ch"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null | python3 -c "
import sys, json
for line in sys.stdin:
    e = json.loads(line)
    crit = ' CRIT' if e.get('crit') else ''
    blocked = ' BLOCKED' if e.get('blocked') else ''
    print(f\"{e['ts'][11:19]} {e.get('src')} -> {e.get('dst')} [{e.get('skill')}] dmg={e.get('dmg')}{crit}{blocked} dst_hp={e.get('dst_hp_before')}->{e.get('dst_hp_after')}\")
" | tail -50

# Skills used
grep '"t":"su"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null | python3 -c "
import sys, json
from collections import Counter
skills = Counter()
for line in sys.stdin:
    e = json.loads(line)
    if e.get('ok'): skills[e.get('skill')] += 1
print('Skills used:', dict(skills.most_common(10)))
"

# Damage received per encounter (group by time proximity)
grep '"t":"ch"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null \
  | python3 -c "
import sys, json
total_in, total_out = 0, 0
for line in sys.stdin:
    e = json.loads(line)
    if e.get('dst') == 'player':
        total_in += e.get('dmg', 0)
    elif e.get('src') == 'player':
        total_out += e.get('dmg', 0)
print(f'Total damage received: {total_in}')
print(f'Total damage dealt: {total_out}')
print(f'D/R ratio: {total_out/(total_in or 1):.1f}')
"
```

## Step 5 — Performance Analysis

```bash
# FPS and tick time over the session
grep '"t":"perf"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null | python3 -c "
import sys, json
entries = [json.loads(l) for l in sys.stdin]
if not entries:
    print('No perf data')
else:
    fpss = [e['fps'] for e in entries if 'fps' in e]
    ticks = [e['tick_ms'] for e in entries if 'tick_ms' in e]
    print(f'FPS: min={min(fpss)} avg={sum(fpss)//len(fpss)} max={max(fpss)}')
    print(f'Tick ms: min={min(ticks)} avg={sum(ticks)//len(ticks)} max={max(ticks)}')
    # Flag bad frames
    bad = [(e['ts'][11:19], e['fps']) for e in entries if e.get('fps',60) < 30]
    if bad: print(f'Low FPS moments ({len(bad)}): {bad[:5]}')
"

# Server tick system breakdown (if server telemetry present)
grep '"t":"tick"' userdata/server/logs/*telemetry*.jsonl 2>/dev/null | tail -20 | python3 -c "
import sys, json
for line in sys.stdin:
    e = json.loads(line)
    sys_str = ' '.join(f'{k}={v}ms' for k,v in (e.get('systems') or {}).items())
    print(f\"{e['ts'][11:19]} total={e.get('ms')}ms entities={e.get('entities')} [{sys_str}]\")
"
```

## Step 6 — Error Correlation

Correlate errors with game state at the time they occurred:

```bash
# Error contexts with preceding state
grep '"t":"err_ctx"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null | python3 -c "
import sys, json
for line in sys.stdin:
    e = json.loads(line)
    print(f\"--- ERROR at {e['ts'][11:19]} ---\")
    print(f\"  Message : {e.get('msg')}\")
    print(f\"  File    : {e.get('file')}\")
    print(f\"  State   : hp={e.get('player_hp')} pos={e.get('pos')} state={e.get('state')}\")
    print(f\"  Breadcrumbs: {' -> '.join(e.get('recent_t',[]))}\")
"

# Cross-reference with err log
echo '--- ERR LOG (last 20 lines) ---'
tail -20 userdata/voxygen/logs/*client_err* 2>/dev/null
```

## Step 7 — World / Environment Context

```bash
# Where the player spent time (biomes, sites)
grep '"t":"wc"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null | python3 -c "
import sys, json
from collections import Counter
biomes, sites = Counter(), Counter()
for line in sys.stdin:
    e = json.loads(line)
    biomes[e.get('biome','?')] += 1
    sites[e.get('site') or 'Open world'] += 1
print('Biomes:', dict(biomes.most_common()))
print('Sites:', dict(sites.most_common()))
"

# NPC behavior (server telemetry)
grep '"t":"npc"' userdata/server/logs/*telemetry*.jsonl 2>/dev/null | python3 -c "
import sys, json
from collections import Counter
transitions = Counter()
for line in sys.stdin:
    e = json.loads(line)
    transitions[f\"{e.get('prev')}->{e.get('next')}\"] += 1
print('NPC state transitions:', dict(transitions.most_common(10)))
"
```

## Step 8 — Build a Timeline Around an Event

When the user reports something happened at a specific moment, reconstruct the ±30 second window:

```bash
# Replace HH:MM:SS with the timestamp of interest
TARGET="14:32:15"
grep -h '"ts"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null | python3 -c "
import sys, json
from datetime import datetime, timedelta
target = datetime.fromisoformat('2026-06-05T${TARGET}Z'.replace('Z','+00:00'))
window = timedelta(seconds=30)
for line in sys.stdin:
    try:
        e = json.loads(line)
        ts = datetime.fromisoformat(e['ts'].replace('Z','+00:00'))
        if abs(ts - target) <= window:
            t = e.get('t','?')
            print(f\"{e['ts'][11:19]} [{t}] {json.dumps({k:v for k,v in e.items() if k not in ('t','ts')})}\")
    except: pass
"
```

## Step 9 — Inventory and Progression

```bash
# All inventory events
grep '"t":"inv"' userdata/voxygen/logs/*telemetry*.jsonl 2>/dev/null | python3 -c "
import sys, json
for line in sys.stdin:
    e = json.loads(line)
    print(f\"{e['ts'][11:19]} {e.get('op'):8} {e.get('item')} x{e.get('qty',1)}\")
"
```

## Step 10 — Full Session Narrative (run all steps)

When you need a complete picture, run steps 2–9 in sequence and synthesize:

1. **Session**: duration, version, character
2. **Survival**: deaths, max health reached, time in combat vs exploration
3. **Combat effectiveness**: DPS dealt vs received, skills used, crit rate
4. **Performance**: average FPS, any lag spikes, tick health
5. **World**: biomes visited, sites entered, time of day range
6. **Errors**: any error_ctx events, correlation with game state
7. **Progression**: items picked up, equipped, consumed

## Event Type Reference

| Code | Meaning | Key fields |
|------|---------|-----------|
| `ss` | Session start | `ver`, `char`, `char_lvl`, `seed` |
| `se` | Session end | `reason`, `duration_s`, `had_errors` |
| `ps` | Player snapshot (5s) | `hp`, `hp_max`, `st`, `en`, `pos`, `state`, `buffs`, `debuffs` |
| `wc` | World context (30s) | `tod`, `weather`, `alt`, `biome`, `site`, `chunk` |
| `ec` | Entity context | `entities[]` with `id`,`kind`,`hp`,`state`,`dist` |
| `ch` | Combat hit | `src`, `dst`, `skill`, `dmg`, `dmg_type`, `crit`, `blocked`, `dst_hp_before/after` |
| `su` | Skill use | `skill`, `energy_cost`, `cooldown_ms`, `ok` |
| `sc` | State change | `from`, `to`, `trigger` |
| `pd` | Player death | `cause`, `killer`, `pos`, `survived_s` |
| `inv` | Inventory change | `op`, `item`, `qty`, `slot` |
| `co` | Chunk op | `op`, `chunk`, `entities`, `ms` |
| `ui` | UI event | `action`, `widget`, `btn` |
| `net` | Network event | `event`, `ms`, `reason` |
| `perf` | Perf snapshot (30s) | `fps`, `frame_ms`, `tick_ms`, `chunks`, `entities`, `draw_calls` |
| `err_ctx` | Error context | `msg`, `file`, `player_hp`, `pos`, `state`, `recent_t` |
| `npc` | NPC decision (server) | `id`, `prev`, `next`, `target`, `reason`, `dist` |
| `trade` | Trade event (server) | `player`, `npc`, `result`, `items_given`, `items_received` |
| `site` | Site event (server) | `event`, `site`, `pos` |
| `tick` | Server tick (50 ticks) | `ms`, `systems{}`, `entities` |
| `lvl` | Character level-up (server) | `event`, `uid`, `new_level` |

## Common Analysis Patterns

**"The player died unexpectedly"** → Steps 3 (ps before death), 4 (combat leading up to pd), 6 (any errors), Step 8 (±30s timeline around pd timestamp)

**"The game felt laggy"** → Step 5 (perf snapshots for FPS dips + tick spikes + draw_call spikes)

**"NPCs were behaving strangely"** → Step 7 (npc transitions), Step 8 (entity context around the weird moment)

**"Something broke after my last change"** → Step 6 (err_ctx), then Step 8 (timeline around the error timestamp)

**"I want to know if the feature I added is working"** → Step 8 (timeline when user would have triggered the feature), then grep for the specific event type the feature should emit

## Notes

- If telemetry files are gzipped (`.jsonl.gz`), decompress first: `gunzip -c file.jsonl.gz | grep ...`
- Telemetry is only generated with `--features xindeler-voxygen/logging-verbose` (dev builds)
- Server telemetry requires `--features xindeler-server-cli/logging-verbose`
- The most recent file may still be open and writing — tail it live with: `tail -f userdata/voxygen/logs/*telemetry*.jsonl`
