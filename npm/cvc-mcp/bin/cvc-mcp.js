#!/usr/bin/env node
'use strict';

require('./release').launch('cvc-mcp').catch(error => {
  console.error(`Failed to install CVC MCP: ${error.message}`);
  process.exit(1);
});
