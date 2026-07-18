import { defineConfig } from "@vscode/test-cli";

export default defineConfig({
  files: "dist/test/**/*.test.js",
  extensionDevelopmentPath: import.meta.dirname,
  workspaceFolder: `${import.meta.dirname}/test-fixtures`,
  mocha: {
    timeout: 10000,
  },
});
