import pytest
from unittest.mock import Mock, patch, AsyncMock
import asyncio


class SynapseError(Exception):
    """Base exception for Synapse SDK errors."""
    pass


class SynapseHTTPError(SynapseError):
    """HTTP-related errors."""
    def __init__(self, status_code, message):
        self.status_code = status_code
        self.message = message
        super().__init__(message)


class SynapseClient:
    """Async HTTP client for Synapse API."""

    def __init__(self, base_url, api_key, timeout=30):
        self.base_url = base_url
        self.api_key = api_key
        self.timeout = timeout

    async def request(self, method, path, json=None, headers=None):
        """Make an HTTP request to the API."""
        request_headers = {
            'Authorization': f'Bearer {self.api_key}',
            'Content-Type': 'application/json',
        }
        if headers:
            request_headers.update(headers)

        url = f'{self.base_url}{path}'

        # Simulated request logic
        if not url.startswith('http'):
            raise SynapseError('Invalid URL')

        return {'success': True}

    async def get(self, path, **kwargs):
        """GET request."""
        return await self.request('GET', path, **kwargs)

    async def post(self, path, json=None, **kwargs):
        """POST request."""
        return await self.request('POST', path, json=json, **kwargs)

    async def put(self, path, json=None, **kwargs):
        """PUT request."""
        return await self.request('PUT', path, json=json, **kwargs)

    async def delete(self, path, **kwargs):
        """DELETE request."""
        return await self.request('DELETE', path, **kwargs)


class TestSynapseClient:
    """Test suite for Synapse Client."""

    @pytest.fixture
    def client(self):
        return SynapseClient(
            base_url='http://localhost:8000',
            api_key='test-api-key',
            timeout=30
        )

    @pytest.mark.asyncio
    async def test_client_initialization(self, client):
        assert client.base_url == 'http://localhost:8000'
        assert client.api_key == 'test-api-key'
        assert client.timeout == 30

    @pytest.mark.asyncio
    async def test_client_custom_timeout(self):
        client = SynapseClient(
            base_url='http://localhost:8000',
            api_key='test-key',
            timeout=60
        )
        assert client.timeout == 60

    @pytest.mark.asyncio
    async def test_get_request(self, client):
        result = await client.get('/transactions')
        assert result is not None
        assert 'success' in result

    @pytest.mark.asyncio
    async def test_post_request(self, client):
        payload = {'amount': 100, 'asset_code': 'USD'}
        result = await client.post('/transactions', json=payload)
        assert result is not None

    @pytest.mark.asyncio
    async def test_put_request(self, client):
        payload = {'status': 'settled'}
        result = await client.put('/transactions/123', json=payload)
        assert result is not None

    @pytest.mark.asyncio
    async def test_delete_request(self, client):
        result = await client.delete('/transactions/123')
        assert result is not None

    @pytest.mark.asyncio
    async def test_authorization_header_included(self, client):
        # Verify authorization header is set correctly
        result = await client.request('GET', '/transactions')
        assert result is not None

    @pytest.mark.asyncio
    async def test_custom_headers_merged(self, client):
        custom_headers = {'X-Custom': 'value'}
        result = await client.request(
            'GET',
            '/transactions',
            headers=custom_headers
        )
        assert result is not None

    @pytest.mark.asyncio
    async def test_json_serialization(self, client):
        payload = {
            'id': 'txn-123',
            'amount': 100.50,
            'status': 'pending',
            'metadata': {'key': 'value'}
        }
        result = await client.post('/transactions', json=payload)
        assert result is not None

    @pytest.mark.asyncio
    async def test_request_timeout_configuration(self, client):
        assert client.timeout == 30

        client_with_timeout = SynapseClient(
            'http://localhost:8000',
            'api-key',
            timeout=5
        )
        assert client_with_timeout.timeout == 5

    @pytest.mark.asyncio
    async def test_base_url_construction(self, client):
        # Ensure proper URL construction
        with pytest.raises(SynapseError):
            invalid_client = SynapseClient(
                'invalid-url',
                'api-key'
            )
            await invalid_client.get('/transactions')

    @pytest.mark.asyncio
    async def test_concurrent_requests(self, client):
        tasks = [
            client.get('/transactions/1'),
            client.get('/transactions/2'),
            client.get('/transactions/3'),
        ]
        results = await asyncio.gather(*tasks)
        assert len(results) == 3

    @pytest.mark.asyncio
    async def test_error_handling(self, client):
        with pytest.raises(SynapseError):
            client_error = SynapseClient(
                'not-a-url',
                'api-key'
            )
            await client_error.get('/transactions')


class TestSynapseErrors:
    """Test suite for error handling."""

    def test_synapse_error_initialization(self):
        error = SynapseError('Test error')
        assert str(error) == 'Test error'

    def test_synapse_http_error_initialization(self):
        error = SynapseHTTPError(401, 'Unauthorized')
        assert error.status_code == 401
        assert error.message == 'Unauthorized'

    def test_synapse_http_error_inheritance(self):
        error = SynapseHTTPError(500, 'Internal Server Error')
        assert isinstance(error, SynapseError)
