import json
from collections import Counter
g=json.load(open('/hermes/kanban/workspaces/t_75f6e158/cpg-ast-sample.json'))
c=Counter()
for n in g['nodes']:
    k=n['kind']
    if k['kind']=='Statement': c['stmt:'+k['data'].get('stmt_kind','?')]+=1
    elif k['kind']=='Expression': c['expr:'+k['data'].get('expr_kind','?')]+=1
for k,v in sorted(c.items()): print(k,v)
print('--- nodes with raw kind names inside code snippets:')
