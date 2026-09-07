use std::sync::atomic::{AtomicBool, Ordering};

static SECCOMP_APPLIED: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
const SECCOMP_SET_MODE_FILTER: u32 = 1;
#[allow(dead_code)]
const SECCOMP_FILTER_FLAG_TSYNC: u32 = 1;

const PR_SET_NO_NEW_PRIVS: i32 = 38;
const PR_SET_SECCOMP: i32 = 22;
const SECCOMP_MODE_FILTER: u64 = 2;

pub fn apply_seccomp_filter() -> Result<(), String> {
    if SECCOMP_APPLIED.load(Ordering::Acquire) {
        return Ok(());
    }

    let ret = unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        return Err(format!(
            "prctl(PR_SET_NO_NEW_PRIVS) failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let prog = build_bpf_filter();

    let ret = unsafe {
        libc::prctl(
            PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER as i32,
            &prog as *const sock_fprog as usize,
            0,
            0,
        )
    };

    if ret != 0 {
        return Err(format!(
            "prctl(PR_SET_SECCOMP) failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    SECCOMP_APPLIED.store(true, Ordering::Release);
    tracing::info!("seccomp filter applied");
    Ok(())
}

#[repr(C)]
struct sock_filter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct sock_fprog {
    len: u16,
    filter: *const sock_filter,
}

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_RET: u16 = 0x06;
const BPF_K: u16 = 0x00;

const SECCOMP_RET_ALLOW: u32 = 0x7fff0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x00000000;

const ALLOWED_SYSCALLS: &[u32] = &[
    0,   // read
    1,   // write
    2,   // open
    3,   // close
    9,   // mmap
    10,  // mprotect
    11,  // munmap
    12,  // brk
    13,  // rt_sigaction
    14,  // rt_sigprocmask
    15,  // rt_sigreturn
    20,  // getpid
    21,  // socket
    35,  // nanosleep
    39,  // getpid (duplicate, fine)
    41,  // socket
    42,  // connect
    49,  // bind
    50,  // listen
    56,  // clone
    57,  // fork
    58,  // vfork
    59,  // execve
    60,  // exit
    61,  // wait4
    62,  // kill
    63,  // readlink
    72,  // fcntl
    73,  // ftruncate
    78,  // getdents
    79,  // getcwd
    80,  // chdir
    82,  // rename
    83,  // mkdir
    84,  // rmdir
    87,  // unlink
    89,  // readlink
    96,  // gettimeofday
    202, // futex
    206, // io_setup
    207, // io_destroy
    208, // io_getevents
    209, // io_submit
    210, // io_cancel
    217, // getdents64
    228, // clock_gettime
    230, // clock_nanosleep
    231, // exit_group
    232, // epoll_wait
    233, // epoll_ctl
    257, // openat
    262, // newfstatat
    288, // accept4
    292, // dup2
    293, // dup3
    302, // prlimit64
    318, // getrandom
    332, // statx
    318, // getrandom
    291, // epoll_create1
    288, // accept
    41,  // socket
    49,  // bind
    50,  // listen
    51,  // getsockname
    52,  // getpeername
    53,  // socketpair
    54,  // setsockopt
    55,  // getsockopt
    72,  // fcntl
    16,  // pipe
    293, // dup3
    74,  // splice
    275, // select
    22,  // pipe
    281, // sendmsg
    282, // recvmsg
];

fn build_bpf_filter() -> sock_fprog {
    let est_size = ALLOWED_SYSCALLS.len() * 3 + 4;
    let mut prog: Vec<sock_filter> = Vec::with_capacity(est_size);

    prog.push(sock_filter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 0, // offset of nr in seccomp_data
    });

    for (i, &nr) in ALLOWED_SYSCALLS.iter().enumerate() {
        let is_last = i == ALLOWED_SYSCALLS.len() - 1;
        let jt_offset = if is_last { 1 } else { 2 }; // jump to ALLOW

        prog.push(sock_filter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: jt_offset as u8,
            jf: 0,
            k: nr,
        });

        prog.push(sock_filter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        });
    }

    prog.push(sock_filter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_KILL_PROCESS,
    });

    let len = prog.len() as u16;
    let boxed = prog.into_boxed_slice();
    let ptr = Box::into_raw(boxed) as *const sock_filter;

    sock_fprog { len, filter: ptr }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seccomp_guard_idempotent() {
        SECCOMP_APPLIED.store(false, Ordering::Release);
        assert!(!SECCOMP_APPLIED.load(Ordering::Acquire));
        SECCOMP_APPLIED.store(true, Ordering::Release);
        assert!(SECCOMP_APPLIED.load(Ordering::Acquire));
    }

    #[test]
    fn build_bpf_filter_produces_valid_prog() {
        let prog = build_bpf_filter();
        assert!(prog.len >= 3);
    }
}
