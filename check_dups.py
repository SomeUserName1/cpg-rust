import subprocess, json
from collections import Counter
subprocess.run(['cargo','run','--quiet','--','sample-crate','-o','/tmp/full.json'],check=True,cwd='/home/someusername/.hermes/kanban/workspaces/t_17450c16')
g=json.load(open('/tmp/full.json'))
ids=[e['id'] for e in g['edges']]
c=Counter(ids)
dups={k:v for k,v in c.items() if v>1}
print(len(g['edges']), 'edges', len(g['nodes']),'nodes', len(dups),'dup ids', list(dups.items())[:5])
for e in g['edges']:
    if e['id'] in dups:
        print(e['id'], e['kind'])
