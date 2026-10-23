// Transcriber.app's executable. macOS grants the microphone and Accessibility per app, and
// charges a process's use of them to the app that launched it. Started from a hotkey tool
// (Karabiner) that app can't hold either permission, so the transcriber hears silence and
// can't paste. Opening this app instead makes it the one macOS asks about.
//
// Runs scripts/toggle_transcribe.sh, then stays alive while the transcriber it started runs,
// so the transcriber keeps a live parent app. Open it with `open -n -g` so a second press
// starts a new instance (which stops the transcriber) and focus stays on your window.
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef REPO
#error "build with -DREPO=\"/path/to/transcriber\" (see build.sh)"
#endif

int main(void) {
    const char *script = REPO "/scripts/toggle_transcribe.sh";
    const char *pid_path = REPO "/scripts/.transcribe.pid";

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        execl("/bin/bash", "bash", script, (char *)NULL);
        perror("execl");
        _exit(127);
    }
    int status;
    if (waitpid(child, &status, 0) < 0) {
        perror("waitpid");
        return 1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
        return 1;

    // The toggle either started the transcriber (PID file present) or stopped it (absent).
    FILE *f = fopen(pid_path, "r");
    if (!f)
        return 0;
    long pid = 0;
    int got = fscanf(f, "%ld", &pid);
    fclose(f);
    if (got != 1 || pid <= 0)
        return 0;
    while (kill((pid_t)pid, 0) == 0)
        sleep(1);
    return 0;
}
