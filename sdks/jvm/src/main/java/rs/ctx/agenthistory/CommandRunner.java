package rs.ctx.agenthistory;

/** Executes a local CLI command. Tests can inject this to avoid spawning ctx. */
@FunctionalInterface
public interface CommandRunner {
    CommandResult run(CommandRequest request) throws Exception;
}

