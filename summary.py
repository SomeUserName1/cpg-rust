import json
from collections import Counter
g=json.load(open('/home/someusername/.hermes/kanban/workspaces/t_17450c16/cpg-full.json'))
ov=Counter()
for e in g['edges']:
    k=e['kind']
    if 'overlay' in k:
        ov[k['overlay']]+=1
    else:
        ov['RAW:'+next(iter(k))]+=1
print(len(g['nodes']),'nodes',len(g['edges']),'edges')
for k,v in ov.most_common(): print(k,v)
print('--- dfg/call samples')
n=0
for e in g['edges']:
    k=e['kind']
    if 'overlay' not in k and next(iter(k))!='Ast':
        print(e['id'],k)
        n+=1
        if n>10: break
