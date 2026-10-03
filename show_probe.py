import json
g=json.load(open('/hermes/kanban/workspaces/t_17450c16/probe.json'))
nodes={n['id']:n for n in g['nodes']}
ch={}
for e in g['edges']:
    if e['kind']['overlay']=='Ast':
        ch.setdefault(e['src'],[]).append((e['kind']['order'],e['kind']['field'],e['dst']))
def show(id,depth=0,maxd=5):
    n=nodes[id]
    k=n['kind']; det=k.get('data',{})
    extra=det.get('stmt_kind') or det.get('expr_kind') or ''
    print('  '*depth+f"{id} {k['kind']} {extra} {n['common']['code'][:45]!r}")
    if depth<maxd:
        for o,f,c in sorted(ch.get(id,[])):
            print('  '*(depth+1)+f"[{o},{f}]")
            show(c,depth+2,maxd)
for n in g['nodes']:
    if n['kind']['kind']=='Statement':
        sk=n['kind']['data'].get('stmt_kind')
        if sk in ('for','while','loop'):
            print('====',sk)
            show(n['id'],maxd=2)
        if sk=='match':
            print('==== match')
            show(n['id'],maxd=4)
