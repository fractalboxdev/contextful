#!/bin/sh
# Write the project declaration and store configuration the Durable Object passed, then
# serve on an empty disk. Serve's stderr goes to /work/serve.log, which the Durable Object
# reads for the pull and mapping figures.
set -eu
cd /work
mkdir -p ".contextful/context/$BENCH_PROJECT"
printf '%s' "$BENCH_STORE_CONFIG" > ".contextful/context/$BENCH_PROJECT/config.toml"
printf '%s' "$BENCH_DECLARATION" > contextful.toml
exec /usr/local/bin/contextful serve --http "0.0.0.0:$BENCH_PORT" --project "$BENCH_PROJECT" \
  --public-key "$ISSUER_PUBLIC_KEY" --audience "$BENCH_AUDIENCE" --max-in-flight 8 2> /work/serve.log
