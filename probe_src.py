import json
P='/hermes/kanban/workspaces/t_17450c16/forprobe.json'
import subprocess
src = '''
fn f(v: Vec<u64>, n: u64) -> u64 {
    let mut s = 0;
    for x in v {
        s = s + x;
    }
    while s < n {
        s = s + 1;
    }
    loop {
        s = s + 1;
        break;
    }
    match s {
        0 => 100,
        m if m % 2 == 0 => 200,
        _ => 300,
    }
}
'''
open('/tmp/probe.rs','w').write(src)
PYEOF
