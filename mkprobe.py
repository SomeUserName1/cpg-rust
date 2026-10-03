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
import os
os.makedirs('probe', exist_ok=True)
open('probe/lib.rs','w').write(src)
