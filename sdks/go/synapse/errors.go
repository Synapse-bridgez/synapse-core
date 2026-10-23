package synapse

import "fmt"

// SynapseError is the canonical error type returned by all SDK methods.
//
// All errors returned by the SDK wrap this type. Use errors.As to inspect:
//
//	var se *synapse.SynapseError
//	if errors.As(err, &se) {
//	    fmt.Println(se.StatusCode, se.Message)
//	}
type SynapseError struct {
	// HTTP status code, or 0 for network-level errors.
	StatusCode int
	// Human-readable message from the server or SDK.
	Message string
	// Optional machine-readable error code from the server.
	Code string
}

func (e *SynapseError) Error() string {
	if e.Code != "" {
		return fmt.Sprintf("synapse: HTTP %d %s [%s]", e.StatusCode, e.Message, e.Code)
	}
	return fmt.Sprintf("synapse: HTTP %d %s", e.StatusCode, e.Message)
}

// IsNotFound returns true when the error represents an HTTP 404.
func IsNotFound(err error) bool {
	se, ok := err.(*SynapseError)
	return ok && se.StatusCode == 404
}

// IsUnauthorized returns true when the error represents an HTTP 401.
func IsUnauthorized(err error) bool {
	se, ok := err.(*SynapseError)
	return ok && se.StatusCode == 401
}

// IsForbidden returns true when the error represents an HTTP 403.
func IsForbidden(err error) bool {
	se, ok := err.(*SynapseError)
	return ok && se.StatusCode == 403
}

// IsInvalidCursor returns true when the error indicates a bad pagination cursor.
func IsInvalidCursor(err error) bool {
	se, ok := err.(*SynapseError)
	return ok && se.StatusCode == 400 && se.Code == "INVALID_CURSOR"
}
