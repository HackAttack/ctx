package rs.ctx.agenthistory;

/** Result returned by a local CLI command runner. */
public final class CommandResult {
    private final String stdout;
    private final String stderr;
    private final int exitCode;

    public CommandResult(String stdout, String stderr, int exitCode) {
        this.stdout = stdout == null ? "" : stdout;
        this.stderr = stderr == null ? "" : stderr;
        this.exitCode = exitCode;
    }

    public String stdout() {
        return stdout;
    }

    public String stderr() {
        return stderr;
    }

    public int exitCode() {
        return exitCode;
    }
}

