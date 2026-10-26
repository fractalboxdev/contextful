# Glossary

Each term names one concept in the corpus.

## fence

A token the storage compares on every write, so a holder that lost its lease cannot commit.
Its use: {{store.lease}}.

## frontier

The highest commit sequence a snapshot covers.
