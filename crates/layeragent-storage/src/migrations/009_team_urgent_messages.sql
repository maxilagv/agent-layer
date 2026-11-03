-- Urgent messages are honoured between a recipient's tool calls: the rest of
-- its current batch is not executed and the next round reads the message.
ALTER TABLE team_messages ADD COLUMN urgent INTEGER NOT NULL DEFAULT 0;
