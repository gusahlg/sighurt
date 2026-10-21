/* Shows that WTF reads JSC_SIGNAL_FOR_GC once, while JSC's option parser rejects it.
   Parent: set the variable, initialize JSC (logs the error, installs the signal), unset it, exec a child.
   Child: initialize JSC with whatever environment it inherited. */
#include <dlfcn.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

typedef int (*set_bool_fn)(const char *, int);
typedef int (*get_bool_fn)(const char *, int *);

static void show_handler(const char *who, int sig)
{
    struct sigaction old;
    if (sigaction(sig, NULL, &old) != 0) {
        printf("%s: sig %d sigaction errno\n", who, sig);
        return;
    }
    printf("%s: sig %d flags=0x%x handler=%p sigaction=%p default=%d\n", who, sig,
        old.sa_flags, (void *)old.sa_handler, (void *)old.sa_sigaction,
        old.sa_handler == SIG_DFL);
}

static void init_jsc(void *lib)
{
    set_bool_fn setb = dlsym(lib, "jsc_options_set_boolean");
    get_bool_fn getb = dlsym(lib, "jsc_options_get_boolean");
    int now = 0;
    if (!setb || !getb) {
        fprintf(stderr, "dlsym: %s\n", dlerror());
        exit(2);
    }
    /* A real option. The first call runs JSC::initialize: WTF::initialize, then Options::initialize. */
    if (!setb("usePollingTraps", 1) || !getb("usePollingTraps", &now) || !now) {
        fprintf(stderr, "usePollingTraps did not stick\n");
        exit(3);
    }
}

int main(int argc, char **argv)
{
    const char *libpath = getenv("WPE_LIB");
    int child = argc > 1 && strcmp(argv[1], "child") == 0;
    void *lib;
    if (!libpath) {
        fprintf(stderr, "WPE_LIB unset\n");
        return 2;
    }
    if (!child)
        setenv("JSC_SIGNAL_FOR_GC", "12", 1);
    lib = dlopen(libpath, RTLD_NOW | RTLD_LOCAL);
    if (!lib) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return 2;
    }
    init_jsc(lib);
    printf("%s: JSC_SIGNAL_FOR_GC=%s\n", child ? "child" : "parent",
        getenv("JSC_SIGNAL_FOR_GC") ? getenv("JSC_SIGNAL_FOR_GC") : "(unset)");
    show_handler(child ? "child" : "parent", SIGUSR1);
    show_handler(child ? "child" : "parent", SIGUSR2);
    fflush(stdout);
    if (child)
        return 0;
    unsetenv("JSC_SIGNAL_FOR_GC");
    pid_t pid = fork();
    if (pid == 0) {
        execl(argv[0], argv[0], "child", (char *)NULL);
        perror("execl");
        _exit(127);
    }
    int st = 1;
    waitpid(pid, &st, 0);
    return WIFEXITED(st) ? WEXITSTATUS(st) : 1;
}
