(function () {
  function probe(ctx) {
    const response = ctx.host.command.run({program: 'coderabbit', args: ['usage'], timeoutMs: 15000});
    const text = (response.stdout + '\n' + response.stderr).replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '');
    if (/not authenticated|please log in|auth login|authentication required|unauthorized|no session found/i.test(text))
      throw {code: 'missing-auth', message: 'Run coderabbit auth login to sign in.'};
    if (response.status !== 0) throw 'CodeRabbit usage command failed (exit ' + response.status + ').';
    const fields = Object.create(null);
    for (const line of text.split(/\r?\n/)) {
      const colon = line.indexOf(':');
      if (colon < 0) continue;
      const label = line.slice(0, colon).trim().toLowerCase(), value = line.slice(colon + 1).trim();
      if (value && !fields[label]) fields[label] = value;
    }
    const lines = [];
    if (/^\d+$/.test(fields['your reviews'] || '') && Number.isSafeInteger(Number(fields['your reviews'])))
      lines.push(ctx.line.text({label: 'Reviews', value: fields['your reviews']}));
    for (const [key, label] of [['usage billing', 'Usage billing'], ['period resets', 'Period resets']])
      if (fields[key]) lines.push(ctx.line.text({label, value: fields[key]}));
    if (!lines.length) throw 'CodeRabbit did not report review counts or billing information.';
    if (fields.organization) lines.push(ctx.line.text({label: 'Organization', value: fields.organization}));
    return {displayName: 'CodeRabbit', source: 'cli', plan: fields.plan, lines};
  }
  globalThis.__usagestat_plugin = {id: 'coderabbit', probe};
})();
