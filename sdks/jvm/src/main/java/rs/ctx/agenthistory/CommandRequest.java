package rs.ctx.agenthistory;

import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Request passed to a local CLI command runner. */
public final class CommandRequest {
    private final String command;
    private final List<String> args;
    private final Path cwd;
    private final Map<String, String> env;
    private final long timeoutMillis;

    public CommandRequest(String command, List<String> args, Path cwd, Map<String, String> env, long timeoutMillis) {
        this.command = command;
        this.args = Collections.unmodifiableList(new ArrayList<>(args));
        this.cwd = cwd;
        this.env = Collections.unmodifiableMap(new LinkedHashMap<>(env));
        this.timeoutMillis = timeoutMillis;
    }

    public String command() {
        return command;
    }

    public List<String> args() {
        return args;
    }

    public Path cwd() {
        return cwd;
    }

    public Map<String, String> env() {
        return env;
    }

    public long timeoutMillis() {
        return timeoutMillis;
    }
}

