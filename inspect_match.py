import json, sys
P='/hermes/kanban/workspaces/t_75f6e158/cpg-ast-sample.json'
g=json.load(open(P))
nodes={n['id']:n for n in g['nodes']}
ch={}
for e in g['edges']:
    if e['kind']['overlay']=='Ast':
        ch.setdefault(e['src'],[]).append((e['kind']['order'],e['kind']['field'],e['dst']))
def show(id,depth=0,maxd=6):
    n=nodes[id]
    k=n['kind']; det=k.get('data',{})
    extra=det.get('stmt_kind') or det.get('expr_kind') or ''
    print('  '*depth+f"{id} {k['kind']} {extra} {n['common']['code'][:60]!r}")
    if depth<maxd:
        for o,f,c in sorted(ch.get(id,[])):
            print('  '*(depth+1)+f"[{o},{f}]")
            show(c,depth+2,maxd)
show(185)   # the match stmt
print('====')
show(152, maxd=3)  # if with return
print('==== functions:')
for n in g['nodes']:
    if n['kind']['kind']=='Function':
        print(n['id'], n['kind']['data'], n['common']['span']['file'])
