import json, subprocess, os
os.makedirs('/hermes/kanban/workspaces/t_17450c16/probe', exist_ok=True)
src='''fn f(x: u64) -> u64 {
    if x == 0 {
        panic!("zero");
    }
    x
}
'''
open('/hermes/kanban/workspaces/t_17450c16/probe/lib.rs','w').write(src)
subprocess.run(['cargo','run','-q','--bin','cpg-ast','--','probe','-o','probe.json'],cwd='/hermes/kanban/workspaces/t_17450c16')
g=json.load(open('/hermes/kanban/workspaces/t_17450c16/probe.json'))
for n in g['nodes']:
    print(n['id'], n['kind'], repr(n['common']['code'][:30]))
