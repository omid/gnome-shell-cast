import js from '@eslint/js';
import globals from 'globals';

export default [
    js.configs.recommended,
    {
        files: ['**/*.js'],
        languageOptions: {
            sourceType: 'module',
            ecmaVersion: 2024,
            globals: {
                ...globals.es2024,
                // GJS globals
                globalThis: 'readonly',
                console: 'readonly',
                TextDecoder: 'readonly',
                TextEncoder: 'readonly',
                setTimeout: 'readonly',
                clearTimeout: 'readonly',
                setInterval: 'readonly',
                clearInterval: 'readonly',
            },
        },
        rules: {
            'no-var': 'error',
            'no-eval': 'error',
            'no-implied-eval': 'error',
            'no-else-return': 'error',
            'no-lonely-if': 'error',
            'no-duplicate-imports': 'error',
            'no-unused-vars': ['error', { argsIgnorePattern: '^_' }],
            'prefer-const': 'error',
            'eqeqeq': 'error',
            'dot-notation': 'error',
        },
    },
];
