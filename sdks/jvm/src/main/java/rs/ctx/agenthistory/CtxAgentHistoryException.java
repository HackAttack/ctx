package rs.ctx.agenthistory;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;

/** Base class for structured ctx agent history SDK errors. */
public class CtxAgentHistoryException extends RuntimeException {
    private final String code;
    private final boolean retryable;
    private final Map<String, Object> details;

    public CtxAgentHistoryException(String code, String message) {
        this(code, message, false, Collections.emptyMap(), null);
    }

    public CtxAgentHistoryException(
            String code,
            String message,
            boolean retryable,
            Map<String, Object> details,
            Throwable cause) {
        super(message, cause);
        this.code = code;
        this.retryable = retryable;
        this.details = Collections.unmodifiableMap(new LinkedHashMap<>(details));
    }

    public String code() {
        return code;
    }

    public boolean retryable() {
        return retryable;
    }

    public Map<String, Object> details() {
        return details;
    }

    public Map<String, Object> asMap() {
        Map<String, Object> out = new LinkedHashMap<>();
        out.put("code", code);
        out.put("message", getMessage());
        out.put("retryable", retryable);
        out.put("details", details);
        out.put("cause", getCause() == null ? null : getCause().getMessage());
        return out;
    }

    public static final class Validation extends CtxAgentHistoryException {
        public Validation(String message) {
            super("invalid_request", message, false, Collections.emptyMap(), null);
        }
    }

    public static final class Protocol extends CtxAgentHistoryException {
        public Protocol(String message, Map<String, Object> details, Throwable cause) {
            super("decode_error", message, false, details, cause);
        }
    }

    public static final class Unsupported extends CtxAgentHistoryException {
        public Unsupported(String message, Map<String, Object> details) {
            super("not_supported", message, false, details, null);
        }
    }

    public static final class Cli extends CtxAgentHistoryException {
        private final String command;
        private final java.util.List<String> args;
        private final int exitCode;
        private final String stdout;
        private final String stderr;

        public Cli(
                String message,
                String command,
                java.util.List<String> args,
                int exitCode,
                String stdout,
                String stderr,
                Throwable cause) {
            this("adapter_error", message, false, command, args, exitCode, stdout, stderr, cause);
        }

        public Cli(
                String code,
                String message,
                boolean retryable,
                String command,
                java.util.List<String> args,
                int exitCode,
                String stdout,
                String stderr,
                Throwable cause) {
            super(code, message, retryable, cliDetails(command, args, exitCode, stdout, stderr), cause);
            this.command = command;
            this.args = Collections.unmodifiableList(new java.util.ArrayList<>(args));
            this.exitCode = exitCode;
            this.stdout = stdout == null ? "" : stdout;
            this.stderr = stderr == null ? "" : stderr;
        }

        public String command() {
            return command;
        }

        public java.util.List<String> args() {
            return args;
        }

        public int exitCode() {
            return exitCode;
        }

        public String stdout() {
            return stdout;
        }

        public String stderr() {
            return stderr;
        }

        private static Map<String, Object> cliDetails(
                String command,
                java.util.List<String> args,
                int exitCode,
                String stdout,
                String stderr) {
            Map<String, Object> details = new LinkedHashMap<>();
            details.put("command", command);
            details.put("args", new java.util.ArrayList<>(args));
            details.put("exitCode", exitCode);
            details.put("stdout", stdout == null ? "" : stdout);
            details.put("stderr", stderr == null ? "" : stderr);
            return details;
        }
    }
}
