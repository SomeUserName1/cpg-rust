import json, subprocess
subprocess.run(['cargo','run','-q','--','probe','-o','probe.json'],cwd='/hermes/kanban/workspaces/t_17450c16')
g=json.load(open('/hermes/kanban/workspaces/t_17450c16/probe.json'))
src='''fn f(v: Vec<u64>, n: u64) -> u64 {
    let mut s = 0;
    while s < n {
        s = s + 1;
    }
    s
}
'''
import os
os.makedirs('/hermes/kanban/workspaces/t_17450c16/probe',exist_ok=True)
open('/hermes/kanban/workspaces/t_17450c16/probe/lib.rs','w').write(src)
