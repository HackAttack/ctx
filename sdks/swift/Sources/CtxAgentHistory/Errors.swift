import Foundation

public struct CtxAgentHistorySDKError: Error, Equatable, Sendable, CustomStringConvertible {
    public var code: AgentHistoryErrorCode
    public var message: String
    public var retryable: Bool
    public var details: JSONValue?
    public var cause: String?
    public var command: [String]?
    public var exitCode: Int?
    public var stdout: String?
    public var stderr: String?

    public init(
        code: AgentHistoryErrorCode,
        message: String,
        retryable: Bool = false,
        details: JSONValue? = nil,
        cause: String? = nil,
        command: [String]? = nil,
        exitCode: Int? = nil,
        stdout: String? = nil,
        stderr: String? = nil
    ) {
        self.code = code
        self.message = message
        self.retryable = retryable
        self.details = details
        self.cause = cause
        self.command = command
        self.exitCode = exitCode
        self.stdout = stdout
        self.stderr = stderr
    }

    public var description: String {
        message
    }

    public var contractError: AgentHistoryContractError {
        AgentHistoryContractError(
            code: code,
            message: message,
            retryable: retryable,
            details: details,
            cause: cause
        )
    }
}
