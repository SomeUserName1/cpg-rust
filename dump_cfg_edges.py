import json
g=json.load(open('/hermes/kanban/workspaces/t_17450c16/probe.json'))
cfg_edges=[e for e in g['edges'] if e['kind']['overlay']=='Cfg']
nodes={n['id']:n for n in g['nodes']}
# Simulate what the CFG pass would do — instead, just dump statement codes to debug tail flow
# Actually easier: run the real builder via cargo test? Just print candidate mapping.
for e in cfg_edges:
    print(e['src'],'->',e['dst'])
