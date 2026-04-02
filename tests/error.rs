use mra::error::*;

#[test]
fn agent_error_classification() {
    assert_eq!(AgentError::Timeout.classification(), ErrorClass::Transient);
    assert_eq!(
        AgentError::Cancelled.classification(),
        ErrorClass::Cancelled
    );
    assert_eq!(
        AgentError::BudgetExceeded.classification(),
        ErrorClass::BudgetExceeded
    );
    assert_eq!(
        AgentError::HandlerFailed("oops".into()).classification(),
        ErrorClass::Permanent
    );
}

#[test]
fn tool_error_classification() {
    assert_eq!(
        ToolError::ResourceExhausted.classification(),
        ErrorClass::Overload
    );
    assert_eq!(
        ToolError::WasmTrap("trap".into()).classification(),
        ErrorClass::Permanent
    );
    assert_eq!(
        ToolError::NotFound("x".into()).classification(),
        ErrorClass::Permanent
    );
}

#[test]
fn llm_error_classification() {
    assert_eq!(LlmError::Timeout.classification(), ErrorClass::Transient);
    assert_eq!(LlmError::RateLimit.classification(), ErrorClass::Overload);
    assert_eq!(
        LlmError::ApiError {
            status: 500,
            message: "fail".into()
        }
        .classification(),
        ErrorClass::Transient
    );
    assert_eq!(
        LlmError::ApiError {
            status: 400,
            message: "bad".into()
        }
        .classification(),
        ErrorClass::Permanent
    );
}

#[test]
fn budget_error_classification() {
    assert_eq!(
        BudgetError::TokenLimitExceeded.classification(),
        ErrorClass::BudgetExceeded
    );
    assert_eq!(
        BudgetError::AdmissionDenied.classification(),
        ErrorClass::Overload
    );
}

#[test]
fn agent_error_unavailable_is_transient() {
    assert_eq!(
        AgentError::Unavailable.classification(),
        ErrorClass::Transient
    );
}

#[test]
fn mra_error_from_agent_error() {
    let agent_err = AgentError::Timeout;
    let mra_err: MraError = agent_err.into();
    assert!(matches!(mra_err, MraError::Agent(_)));
}

#[test]
fn mra_error_from_tool_error() {
    let tool_err = ToolError::ResourceExhausted;
    let mra_err: MraError = tool_err.into();
    assert!(matches!(mra_err, MraError::Tool(_)));
}

#[test]
fn errors_implement_display() {
    let err = AgentError::Timeout;
    let msg = err.to_string();
    assert!(!msg.is_empty());
}

#[test]
fn test_agent_error_llm_variant_preserves_classification() {
    let llm_err = LlmError::RateLimit;
    let agent_err = AgentError::Llm(llm_err);
    assert_eq!(agent_err.classification(), ErrorClass::Overload);
}

#[test]
fn test_agent_error_llm_timeout_is_transient() {
    let agent_err = AgentError::Llm(LlmError::Timeout);
    assert_eq!(agent_err.classification(), ErrorClass::Transient);
}

#[test]
fn tool_error_invalid_args_is_permanent() {
    assert_eq!(
        ToolError::InvalidArgs("bad input".into()).classification(),
        ErrorClass::Permanent
    );
}

#[test]
fn agent_error_tool_variant_preserves_classification() {
    let tool_err = ToolError::ExecutionFailed("cmd failed".into());
    let agent_err = AgentError::Tool(tool_err);
    assert_eq!(agent_err.classification(), ErrorClass::Transient);
}

#[test]
fn errors_implement_std_error() {
    fn assert_std_error<T: std::error::Error>() {}
    assert_std_error::<AgentError>();
    assert_std_error::<ToolError>();
    assert_std_error::<LlmError>();
    assert_std_error::<BudgetError>();
    assert_std_error::<MraError>();
}
