package rs.ctx.agenthistory;

import java.util.Map;

/** Response returned by locate-session operations. */
public final class LocateSessionResponse extends AgentHistoryEnvelope {
    private final LocationResult location;

    LocateSessionResponse(Map<String, Object> canonical) {
        super(canonical);
        this.location = LocationResult.from(payload("location"));
    }

    public LocationResult getLocation() {
        return location;
    }

    public LocationResult location() {
        return location;
    }
}
