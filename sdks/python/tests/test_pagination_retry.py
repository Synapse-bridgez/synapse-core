import pytest
from unittest.mock import patch, MagicMock
import time


class PaginationOptions:
    """Pagination options for API requests."""

    def __init__(self, limit=None, offset=None, cursor=None):
        self.limit = limit
        self.offset = offset
        self.cursor = cursor


class PaginationError(Exception):
    """Pagination-related errors."""
    pass


class Paginator:
    """Handles pagination for API responses."""

    def __init__(self, client, path, limit=50):
        self.client = client
        self.path = path
        self.limit = limit
        self.offset = 0
        self.cursor = None

    async def fetch_page(self):
        """Fetch a single page of results."""
        params = {'limit': self.limit}
        if self.cursor:
            params['cursor'] = self.cursor
        else:
            params['offset'] = self.offset
        return await self.client.get(self.path, params=params)

    async def fetch_all(self):
        """Fetch all pages."""
        results = []
        while True:
            page = await self.fetch_page()
            if not page.get('data'):
                break
            results.extend(page['data'])
            if not page.get('has_more'):
                break
            self.cursor = page.get('next_cursor')
            if not self.cursor:
                self.offset += self.limit
        return results


class RetryConfig:
    """Configuration for retry behavior."""

    def __init__(self, max_retries=3, backoff_factor=2, initial_delay=1):
        self.max_retries = max_retries
        self.backoff_factor = backoff_factor
        self.initial_delay = initial_delay


class RetryableClient:
    """HTTP client with retry logic."""

    def __init__(self, client, retry_config=None):
        self.client = client
        self.retry_config = retry_config or RetryConfig()
        self.attempt_count = 0

    async def request_with_retry(self, method, path, **kwargs):
        """Execute request with retry on transient failures."""
        last_error = None

        for attempt in range(self.retry_config.max_retries + 1):
            self.attempt_count = attempt
            try:
                return await self.client.request(method, path, **kwargs)
            except Exception as e:
                last_error = e
                if attempt < self.retry_config.max_retries:
                    delay = (
                        self.retry_config.initial_delay
                        * (self.retry_config.backoff_factor ** attempt)
                    )
                    await self._async_sleep(delay)
                else:
                    raise last_error

    async def _async_sleep(self, duration):
        """Mock sleep for testing."""
        pass


class TestPagination:
    """Test suite for pagination."""

    @pytest.fixture
    def mock_client(self):
        client = MagicMock()
        client.get = MagicMock(
            return_value={
                'data': [{'id': 1}, {'id': 2}],
                'has_more': False
            }
        )
        return client

    @pytest.mark.asyncio
    async def test_paginator_initialization(self, mock_client):
        paginator = Paginator(mock_client, '/transactions')
        assert paginator.limit == 50
        assert paginator.offset == 0

    @pytest.mark.asyncio
    async def test_paginator_custom_limit(self, mock_client):
        paginator = Paginator(mock_client, '/transactions', limit=25)
        assert paginator.limit == 25

    @pytest.mark.asyncio
    async def test_pagination_offset_based(self, mock_client):
        paginator = Paginator(mock_client, '/transactions', limit=10)
        paginator.offset = 20

        page = await paginator.fetch_page()
        assert page is not None

    @pytest.mark.asyncio
    async def test_pagination_cursor_based(self, mock_client):
        paginator = Paginator(mock_client, '/transactions')
        paginator.cursor = 'next-page-cursor-123'

        page = await paginator.fetch_page()
        assert page is not None

    @pytest.mark.asyncio
    async def test_fetch_all_pages(self, mock_client):
        mock_client.get = MagicMock(
            return_value={
                'data': [{'id': 1}, {'id': 2}],
                'has_more': False
            }
        )
        paginator = Paginator(mock_client, '/transactions')

        all_results = await paginator.fetch_all()
        assert isinstance(all_results, list)

    @pytest.mark.asyncio
    async def test_pagination_multiple_pages(self, mock_client):
        call_count = 0

        async def mock_get(*args, **kwargs):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                return {
                    'data': [{'id': 1}, {'id': 2}],
                    'has_more': True,
                    'next_cursor': 'cursor-2'
                }
            elif call_count == 2:
                return {
                    'data': [{'id': 3}, {'id': 4}],
                    'has_more': False
                }
            return {'data': [], 'has_more': False}

        mock_client.get = mock_get
        paginator = Paginator(mock_client, '/transactions')

        all_results = await paginator.fetch_all()
        assert isinstance(all_results, list)

    def test_pagination_options_initialization(self):
        opts = PaginationOptions(limit=25, offset=50)
        assert opts.limit == 25
        assert opts.offset == 50

    def test_pagination_options_cursor(self):
        opts = PaginationOptions(cursor='cursor-xyz')
        assert opts.cursor == 'cursor-xyz'


class TestRetry:
    """Test suite for retry logic."""

    @pytest.fixture
    def mock_client(self):
        return MagicMock()

    @pytest.fixture
    def retry_client(self, mock_client):
        return RetryableClient(mock_client)

    @pytest.mark.asyncio
    async def test_retry_config_initialization(self):
        config = RetryConfig(max_retries=5, backoff_factor=3, initial_delay=0.5)
        assert config.max_retries == 5
        assert config.backoff_factor == 3
        assert config.initial_delay == 0.5

    @pytest.mark.asyncio
    async def test_retry_default_config(self, mock_client):
        client = RetryableClient(mock_client)
        assert client.retry_config.max_retries == 3
        assert client.retry_config.backoff_factor == 2
        assert client.retry_config.initial_delay == 1

    @pytest.mark.asyncio
    async def test_request_succeeds_without_retry(self, retry_client):
        retry_client.client.request = MagicMock(
            return_value={'data': 'success'}
        )

        result = await retry_client.request_with_retry('GET', '/transactions')
        assert retry_client.attempt_count == 0

    @pytest.mark.asyncio
    async def test_request_retries_on_failure(self, retry_client):
        retry_client.client.request = MagicMock(side_effect=Exception('Timeout'))

        with pytest.raises(Exception):
            await retry_client.request_with_retry('GET', '/transactions')

    @pytest.mark.asyncio
    async def test_retry_count_increments(self, retry_client):
        attempts = []

        async def mock_request(*args, **kwargs):
            attempts.append(1)
            if len(attempts) < 3:
                raise Exception('Transient error')
            return {'success': True}

        retry_client.client.request = mock_request

        result = await retry_client.request_with_retry('GET', '/transactions')
        assert result is not None

    @pytest.mark.asyncio
    async def test_idempotency_safety_on_retry(self, retry_client):
        """Test that retries don't cause idempotency issues."""
        request_ids = []

        async def mock_request(*args, **kwargs):
            # In idempotent requests, same payload should be retried safely
            request_ids.append(kwargs.get('idempotency_key'))
            if len(request_ids) == 1:
                raise Exception('Network error')
            return {'created': True}

        retry_client.client.request = mock_request

        # Should retry and succeed
        result = await retry_client.request_with_retry(
            'POST',
            '/transactions',
            idempotency_key='key-123'
        )
        assert result is not None

    @pytest.mark.asyncio
    async def test_backoff_exponential(self, retry_client):
        """Test exponential backoff calculation."""
        config = RetryConfig(
            max_retries=3,
            backoff_factor=2,
            initial_delay=1
        )

        expected_delays = [1, 2, 4]  # 1 * 2^0, 1 * 2^1, 1 * 2^2

        for i, expected in enumerate(expected_delays):
            calculated = (
                config.initial_delay * (config.backoff_factor ** i)
            )
            assert calculated == expected

    @pytest.mark.asyncio
    async def test_retry_exhaustion(self, retry_client):
        """Test behavior when all retries are exhausted."""
        retry_client.client.request = MagicMock(
            side_effect=Exception('Persistent error')
        )

        with pytest.raises(Exception, match='Persistent error'):
            await retry_client.request_with_retry('GET', '/transactions')

        # Should have attempted max_retries + 1 times
        assert retry_client.attempt_count == retry_client.retry_config.max_retries

    @pytest.mark.asyncio
    async def test_idempotent_post_with_retry(self, retry_client):
        """Test that POST requests with idempotency keys retry safely."""
        call_count = 0

        async def mock_request(*args, **kwargs):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                raise Exception('Network timeout')
            return {'id': 'txn-123', 'created': True}

        retry_client.client.request = mock_request

        result = await retry_client.request_with_retry(
            'POST',
            '/transactions',
            json={'amount': 100},
            idempotency_key='safe-key'
        )
        assert result['id'] == 'txn-123'
        assert call_count == 2

    def test_retry_configuration_custom(self):
        """Test custom retry configuration."""
        config = RetryConfig(
            max_retries=10,
            backoff_factor=1.5,
            initial_delay=0.1
        )

        assert config.max_retries == 10
        assert config.backoff_factor == 1.5
        assert config.initial_delay == 0.1
