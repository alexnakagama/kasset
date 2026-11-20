# Spec Delta

## MODIFIED Requirements

### Requirement: Volume control
The system SHALL adjust volume in steps of 5% between 0% and 100%, starting at the saved volume, or at 100% when no volume is saved.

#### Scenario: Lower volume
- **WHEN** volume is 100% and the user presses `-`
- **THEN** volume becomes 95% and audio is quieter

#### Scenario: Volume bounds
- **WHEN** volume is 100% and the user presses `+`
- **THEN** volume stays at 100%

#### Scenario: Saved volume
- **WHEN** the volume was 60% when kasset last quit
- **THEN** kasset starts at 60%
