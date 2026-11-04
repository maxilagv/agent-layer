-- Cross-model review and consultants.
-- The model alias members without their own run with, as last started.
ALTER TABLE teams ADD COLUMN default_model TEXT;
-- The model each activation ran with: its alias and the model it names
-- (lowercase id without vendor prefix), recorded when the activation starts.
ALTER TABLE team_activations ADD COLUMN model TEXT;
ALTER TABLE team_activations ADD COLUMN model_identity TEXT;
-- The model of the activation that submitted a patch and of the one that
-- reviewed it.
ALTER TABLE team_patches ADD COLUMN author_model TEXT;
ALTER TABLE team_patches ADD COLUMN reviewer_model TEXT;
-- A consultant member: who asked, the question it answers, its token budget
-- and, once retired, how the consultation ended.
ALTER TABLE team_members ADD COLUMN consult_asker TEXT;
ALTER TABLE team_members ADD COLUMN consult_question TEXT;
ALTER TABLE team_members ADD COLUMN consult_budget INTEGER;
ALTER TABLE team_members ADD COLUMN consult_outcome TEXT;
