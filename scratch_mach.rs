pub fn test() {
    let dlopen_ptr = libc::dlopen as *const ();
    let pthread_exit_ptr = libc::pthread_exit as *const ();
    println!("dlopen: {:p}", dlopen_ptr);
    println!("pthread_exit: {:p}", pthread_exit_ptr);
}
