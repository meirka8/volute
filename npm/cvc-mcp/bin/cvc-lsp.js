#!/usr/bin/env node
'use strict';

require('./release').launch('cvc-lsp').catch(error => {
  console.error(`Failed to install CVC LSP: ${error.message}`);
  process.exit(1);
});
