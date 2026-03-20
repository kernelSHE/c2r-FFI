//! Generated from IR (source: calc.c). No FFI.

pub fn compute(x: i32, n: i32) -> i32 {
    let mut acc: i32 = 0;
    let mut i: i32 = 0;
    if n > 0 {
    while i < n {
    acc = acc + x;
    i = i + 1;
    }
    }
    return acc;
}

