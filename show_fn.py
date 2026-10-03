import json
g=json.load(open('/hermes/kanban/workspaces/t_17450c16/probe.json'))
nodes={n['id']:n for n in g['nodes']}
def kids(i):
    out=[]
    for e in g['edges']:
        if e['kind']['overlay']=='Ast' and e['src']==i:
            out.append((e['kind']['order'],e['kind']['field'],e['dst']))
    out.sort()
    return out
def show(i,d=0):
    n=nodes[i]; k=n['kind']
    det=k.get('data',{})
    print('  '*d+f"{i} {k['kind']} {det.get('stmt_kind') or det.get('expr_kind') or ''} {n['common']['code'][:50]!r}")
    for o,f,c in kids(i):
        print('  '*(d+1)+f"[{o},{f}]")
        show(c,d+2)
# find fn f body
for n in g['nodes']:
    if n['kind']['kind']=='Function':
        for o,f,c in kids(n['id']):
            print(f"[{o},{f}]")
            show(c,1)
