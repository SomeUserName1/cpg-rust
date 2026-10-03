import json
g=json.load(open('/hermes/kanban/workspaces/t_75f6e158/cpg-ast-sample.json'))
nodes={n['id']:n for n in g['nodes']}
edges=g['edges']
ast=[e for e in edges if e['kind']['overlay']=='Ast']
ch={}
for e in ast:
    ch.setdefault(e['src'],[]).append((e['kind']['order'],e['kind']['field'],e['dst']))
def show(id,depth=0,maxd=4):
    n=nodes[id]
    k=n['kind']
    det=k.get('data',{})
    extra=det.get('stmt_kind') or det.get('expr_kind') or ''
    print('  '*depth+f"{id} {k['kind']} {extra} code={n['common']['code'][:50]!r}")
    if depth<maxd:
        for o,f,c in sorted(ch.get(id,[])):
            print('  '*(depth+1)+f"[{o},{f}]")
            show(c,depth+2,maxd)
import sys
want=sys.argv[1:] or ['match']
for n in g['nodes']:
    if n['kind']['kind']=='Statement':
        sk=n['kind']['data'].get('stmt_kind')
        if not want or sk in want:
            print('====',sk)
            show(n['id'],maxd=int(len(want)>0 and 3 or 3))
