package rs.ctx.agenthistory;

/** Placeholder configuration for a future hosted agent-history-v1 backend. */
public final class HostedConfig {
    private final String baseUrl;
    private final String apiKey;

    private HostedConfig(Builder builder) {
        this.baseUrl = builder.baseUrl;
        this.apiKey = builder.apiKey;
    }

    public String baseUrl() {
        return baseUrl;
    }

    public String apiKey() {
        return apiKey;
    }

    public static Builder builder() {
        return new Builder();
    }

    public static final class Builder {
        private String baseUrl;
        private String apiKey;

        public Builder baseUrl(String baseUrl) {
            this.baseUrl = baseUrl;
            return this;
        }

        public Builder apiKey(String apiKey) {
            this.apiKey = apiKey;
            return this;
        }

        public HostedConfig build() {
            return new HostedConfig(this);
        }
    }
}

