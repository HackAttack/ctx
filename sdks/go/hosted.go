package ctxagenthistory

import "context"

// HostedConfig reserves the hosted agent-history-v1 configuration surface.
type HostedConfig struct {
	BaseURL string
	APIKey  string
}

// NewHostedClient creates a placeholder hosted client. Operations return
// ErrorKindHostedNotImplemented without making network calls.
func NewHostedClient(config HostedConfig) *Client {
	return NewClient(WithTransport(hostedTransport{config: config}))
}

type hostedTransport struct {
	config HostedConfig
}

func (h hostedTransport) Do(_ context.Context, op Operation) ([]byte, error) {
	if op.Name == "version" {
		return []byte(""), nil
	}
	details := Object{"backend": "hosted"}
	if h.config.BaseURL != "" {
		details["baseUrl"] = h.config.BaseURL
	}
	return nil, &Error{
		Kind:    ErrorKindHostedNotImplemented,
		Message: "hosted ctx agent history backend is not available in this in-repo SDK",
		Err: &AgentHistoryError{
			Code:      ErrorKindHostedNotImplemented,
			Message:   "hosted ctx agent history backend is not available in this in-repo SDK",
			Retryable: false,
			Details:   details,
		},
	}
}
