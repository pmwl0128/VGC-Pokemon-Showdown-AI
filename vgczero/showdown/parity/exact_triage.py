"""Triage an `exact.js --dump` file.

    python exact_triage.py <dump.json>              # failures grouped by first divergence
    python exact_triage.py <dump.json> <id> [max]   # draw-by-draw view and logs of one failure
"""
import collections
import json
import sys


def classify(fails):
    counts = collections.Counter()
    examples = collections.defaultdict(list)
    for f in fails:
        i, sd, en = f['firstDraw'], f['showdownTrace'], f['engineTrace']
        a = sd[i] if 0 <= i < len(sd) else None
        b = en[i] if 0 <= i < len(en) else None
        if i < 0:
            key = 'draws equal, state differs'
        elif a is None:
            key = f'engine extra draw {b}'
        elif b is None:
            key = f'engine missing draw {a[:3]} @ {a[3]}'
        else:
            parts = a[3].split(' > ')
            key = f"SD {a[:2]} vs EN {b[:2]} @ {parts[0]}{' > ' + parts[-1] if len(parts) > 1 else ''}"
        counts[key] += 1
        examples[key].append(f['id'])
    for key, n in counts.most_common(40):
        print(f'{n:4} {key}   e.g. {examples[key][:3]}')


def show(fails, sel, limit):
    shown = 0
    for f in fails:
        if sel != f['id'] and sel not in f['where']:
            continue
        shown += 1
        if shown > limit:
            break
        print(f"=== {f['id']} {f['kind']} first#{f['firstDraw']} sd={f['showdownDraws']} en={f['engineDraws']}")
        sd, en = f['showdownTrace'], f['engineTrace']
        for i in range(max(len(sd), len(en))):
            a = sd[i] if i < len(sd) else None
            b = en[i] if i < len(en) else None
            mark = '  ' if a and b and a[:3] == b[:3] else '!!'
            print(f"  {mark}{i:3} SD {str(a[:3]) if a else '-':16} EN {str(b) if b else '-':16} {a[3] if a else ''}")
        print('  SD log:', ' || '.join(l for l in f['showdownLog'] if l and not l.startswith('|t:'))[:1500])
        print('  EN log:', ' || '.join(f['engineLog'])[:1500])
        print('  choices:', f['choices'])
        for k, a, b in f['stateDiff']:
            print(f'   diff {k}: showdown {a!r} engine {b!r}')


def main():
    with open(sys.argv[1], encoding='utf-8') as fh:
        fails = json.load(fh)
    if len(sys.argv) > 2:
        show(fails, sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 4)
    else:
        classify(fails)


if __name__ == '__main__':
    main()
