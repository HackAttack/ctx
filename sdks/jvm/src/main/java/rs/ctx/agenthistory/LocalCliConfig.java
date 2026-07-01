package rs.ctx.agenthistory;

import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.Map;

/** Configuration for the local ctx CLI adapter. */
public final class LocalCliConfig {
    private final String ctxPath;
    private final String dataRoot;
    private final Path cwd;
    private final Map<String, String> env;
    private final long timeoutMillis;
    private final CommandRunner runner;

    private LocalCliConfig(Builder builder) {
        this.ctxPath = builder.ctxPath;
        this.dataRoot = builder.dataRoot;
        this.cwd = builder.cwd;
        this.env = new LinkedHashMap<>(builder.env);
        this.timeoutMillis = builder.timeoutMillis;
        this.runner = builder.runner;
    }

    public String ctxPath() {
        return ctxPath;
    }

    public String dataRoot() {
        return dataRoot;
    }

    public Path cwd() {
        return cwd;
    }

    public Map<String, String> env() {
        return new LinkedHashMap<>(env);
    }

    public long timeoutMillis() {
        return timeoutMillis;
    }

    public CommandRunner runner() {
        return runner;
    }

    public static Builder builder() {
        return new Builder();
    }

    public static final class Builder {
        private String ctxPath = "ctx";
        private String dataRoot;
        private Path cwd;
        private final Map<String, String> env = new LinkedHashMap<>();
        private long timeoutMillis = 60_000;
        private CommandRunner runner;

        public Builder ctxPath(String ctxPath) {
            this.ctxPath = ctxPath;
            return this;
        }

        public Builder dataRoot(String dataRoot) {
            this.dataRoot = dataRoot;
            return this;
        }

        public Builder cwd(Path cwd) {
            this.cwd = cwd;
            return this;
        }

        public Builder env(String name, String value) {
            this.env.put(name, value);
            return this;
        }

        public Builder env(Map<String, String> env) {
            this.env.putAll(env);
            return this;
        }

        public Builder timeoutMillis(long timeoutMillis) {
            this.timeoutMillis = timeoutMillis;
            return this;
        }

        public Builder runner(CommandRunner runner) {
            this.runner = runner;
            return this;
        }

        public LocalCliConfig build() {
            return new LocalCliConfig(this);
        }
    }
}

