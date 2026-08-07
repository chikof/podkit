/// Unique id of a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserId(pub i64);

/// Unique id of a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleId(pub i64);

/// Unique id of a permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionId(pub i64);

/// Unique id of a team.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamId(pub i64);

/// Unique id of a team membership row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamMemberId(pub i64);

/// Unique id of a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectId(pub i64);

/// Unique id of a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerId(pub i64);

/// Unique id of an application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationId(pub i64);

/// Unique id of a deployment attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeploymentId(pub i64);

/// Unique id of an environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvVarId(pub i64);

/// Unique id of a custom domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomDomainId(pub i64);
