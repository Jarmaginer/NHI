#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <signal.h>
#include <string.h>
#include <errno.h>

void daemonize(const char* output_file) {
    // First fork - create child process
    pid_t pid = fork();
    if (pid > 0) {
        exit(0);  // Parent process exits
    }
    if (pid < 0) {
        perror("First fork failed");
        exit(1);
    }

    // Create new session - become session leader
    if (setsid() < 0) {
        perror("setsid failed");
        exit(1);
    }

    // Ignore SIGHUP signal
    signal(SIGHUP, SIG_IGN);

    // Second fork - ensure we're not session leader
    pid = fork();
    if (pid > 0) {
        exit(0);  // First child process exits
    }
    if (pid < 0) {
        perror("Second fork failed");
        exit(1);
    }

    // Don't change to root directory to avoid path issues
    // Keep current working directory for relative paths to work

    // Set file creation mask
    umask(0);

    // Close all file descriptors
    close(STDIN_FILENO);
    close(STDOUT_FILENO);
    close(STDERR_FILENO);

    // Reopen standard file descriptors
    int fd = open("/dev/null", O_RDWR);  // stdin
    if (fd != STDIN_FILENO) {
        perror("Failed to open /dev/null for stdin");
        exit(1);
    }

    // Open output file for stdout
    fd = open(output_file, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (fd != STDOUT_FILENO) {
        perror("Failed to open output file for stdout");
        exit(1);
    }

    // Duplicate stdout for stderr
    if (dup2(STDOUT_FILENO, STDERR_FILENO) != STDERR_FILENO) {
        perror("Failed to duplicate stdout for stderr");
        exit(1);
    }
}

int main(int argc, char *argv[]) {
    if (argc < 3) {
        fprintf(stderr, "Usage: %s <output_file> <program> [args...]\n", argv[0]);
        fprintf(stderr, "Example: %s /tmp/output.log /bin/sleep 100\n", argv[0]);
        return 1;
    }

    const char* output_file = argv[1];
    const char* program = argv[2];

    // Daemonize the process
    daemonize(output_file);

    // Execute the target program
    execv(program, &argv[2]);

    // If we reach here, execv failed
    perror("execv failed");
    return 1;
}
