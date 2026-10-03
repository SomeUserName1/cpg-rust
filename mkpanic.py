import json, subprocess, os
os.makedirs('/hermes/kanban/workspaces/t_17450c16/probe', exist_ok=True)
src='''fn f(x: u64) -> u64 {
    if x == 0 {
        panic!("zero");
    }
    let y = vec![1][0];
    x
}
'''
open('/hermes/kanban/workspaces/t_17450c16/probe/lib.rs','w').write(src)
subprocess.run(['cargo','run','-q','--','probe','-o','probe.json'],cwd='/hermes/kanban/workspaces/t_17450c16')
g=json.load(open('/hermes/kanban/workspaces/t_17450c16/probe.json'))
for n in g['nodes']:
    k=n['kind']
    if k['kind']=='Expression':
        ek=k['data'].get('expr_kind','')
        if 'panic' in ek or 'raw' in ek or 'error' in ek:
            print(ek, repr(n['common']['code'][:40]), n['common']['span'])
